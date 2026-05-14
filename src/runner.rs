//! Main loop: drive libghostty + the PTY, schedule events, and ship
//! captured frames to raw-frame consumer threads.
//!
//! ## Threading model
//!
//! - **Main thread**: owns the [`Terminal`], the PTY and the [`KeyTranslator`].
//!   It drains the PTY into the terminal each iteration, executes the next
//!   scripted event when its scheduled time arrives, and grabs a screen
//!   snapshot at every framerate tick.
//! - **Raw-frame consumer threads**: optionally receive the same dense raw
//!   frames directly from the runner.
//!
//! Time is computed up‑front: each event in the parsed script gets an
//! absolute deadline. The loop sleeps until the next interesting deadline
//! (event or frame) using `poll(2)` on the PTY fd so any incoming output
//! also wakes us early.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Sender, TrySendError};
use libghostty_vt::{
    Terminal, TerminalOptions,
    key::KittyKeyFlags,
    render::{CellIterator, RenderState, RowIterator},
    style::RgbColor,
    terminal::{Point, PointCoordinate},
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use regex::Regex;
use tracing::{debug, info, trace, warn};

use crate::{
    FrameStyle,
    keys::KeyTranslator,
    pty::{Pty, PtyError, PtySize},
    recording::{CellSnap, RawFrame, style_flags},
    script::{Event, KeyAction, NamedKey, Script, Settings, WaitScope},
};

/// Output of [`crate::run_and_return_recording`].
pub struct RunOutput {
    pub recording: crate::recording::Recording,
    /// Pipeline-health counters captured during the run.
    pub stats: RunStats,
}

/// Pipeline-health counters captured by a single run. All fields are
/// monotonic counters or high-water marks; none of them affect the
/// recording itself.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunStats {
    /// Number of frames the runner intended to capture (one per frame
    /// deadline, including any that were ultimately dropped).
    pub expected_frames: u64,
    /// Number of frames captured by the runner.
    pub captured_frames: u64,
    /// Number of raw-frame consumer channels attached to the run.
    pub raw_frame_consumer_count: usize,
    /// Highest observed `len()` of any runner → raw-frame consumer queue.
    /// Zero when no consumer was attached.
    pub max_raw_frame_consumer_queue_len: usize,
    /// Number of frames the runner couldn't forward to raw-frame consumers
    /// because consumer queues were full.
    pub raw_frame_consumer_dropped_frames: u64,
}

impl RunStats {
    /// Fraction of expected frames that were not captured. Returns 0.0 when no
    /// frames were expected.
    pub fn missed_capture_fraction(&self) -> f64 {
        if self.expected_frames == 0 {
            0.0
        } else {
            self.expected_frames.saturating_sub(self.captured_frames) as f64
                / self.expected_frames as f64
        }
    }

    /// Fraction of raw-frame consumer sends that were dropped. Returns 0.0 when
    /// no consumer frames were expected.
    pub fn dropped_consumer_fraction(&self) -> f64 {
        let expected_consumer_frames = self.expected_frames * self.raw_frame_consumer_count as u64;
        if expected_consumer_frames == 0 {
            0.0
        } else {
            self.raw_frame_consumer_dropped_frames as f64 / expected_consumer_frames as f64
        }
    }
}

/// Total run options derived from the parsed script and CLI overrides.
pub struct RunOptions {
    /// Cell grid size used for the terminal. Derived from
    /// `Settings::{cols, rows}` if set, else from `width/height/font_size`.
    pub cols: u16,
    pub rows: u16,
    /// Per‑cell pixel size used by the GIF renderer (also reported to
    /// libghostty so that pixel‑based queries don't divide by zero).
    pub cell_w_px: u32,
    pub cell_h_px: u32,
    pub frame_style: FrameStyle,
}

pub fn derive_options(s: &Settings) -> RunOptions {
    // Approximate cell metrics – the GIF renderer measures them properly
    // from the loaded font; here we just need something non‑zero for
    // libghostty's resize call.
    let cell_w_px = (s.font_size * 0.5).round().max(1.0) as u32;
    let cell_h_px = (s.font_size * s.line_height).round().max(1.0) as u32;
    let frame_style = FrameStyle {
        canvas_width_px: Some(s.width),
        canvas_height_px: Some(s.height),
        padding_px: s.padding,
        margin_px: s.margin,
        margin_fill: s.margin_fill,
        window_bar: s.window_bar,
        window_bar_size_px: s.window_bar_size,
        border_radius_px: s.border_radius,
    };
    let inner_w = s
        .width
        .saturating_sub((frame_style.padding_px + frame_style.margin_px) * 2);
    let inner_h = s
        .height
        .saturating_sub((frame_style.padding_px + frame_style.margin_px) * 2)
        .saturating_sub(if frame_style.window_bar.enabled() {
            frame_style.window_bar_size_px
        } else {
            0
        });
    let cols = s
        .cols
        .unwrap_or_else(|| (inner_w / cell_w_px).max(20) as u16);
    let rows = s
        .rows
        .unwrap_or_else(|| (inner_h / cell_h_px).max(5) as u16);
    RunOptions {
        cols,
        rows,
        cell_w_px,
        cell_h_px,
        frame_style,
    }
}

/// Run the script end-to-end. Returns only pipeline stats.
pub fn run(script: &Script) -> Result<RunStats> {
    run_with_raw_frame_consumers(script, Vec::new())
}

/// Run the script and optionally mirror dense raw frames into a raw-frame consumer.
///
/// The consumer is attached directly to the terminal-driving thread. Sends are
/// non-blocking, so a slow consumer cannot stall the PTY loop.
pub fn run_with_raw_frame_consumer(
    script: &Script,
    raw_frame_consumer: Option<Sender<RawFrame>>,
) -> Result<RunStats> {
    run_with_raw_frame_consumers(script, raw_frame_consumer.into_iter().collect())
}

/// Run the script and mirror dense raw frames into zero or more raw-frame consumers.
///
/// Each consumer receives frames directly from the terminal-driving thread via
/// `try_send`; full or disconnected consumer queues do not block capture.
pub fn run_with_raw_frame_consumers(
    script: &Script,
    raw_frame_consumers: Vec<Sender<RawFrame>>,
) -> Result<RunStats> {
    enforce_require(&script.require)?;
    let opts = derive_options(&script.settings);
    let pty_size = PtySize {
        cols: opts.cols,
        rows: opts.rows,
        px_w: (opts.cols as u32 * opts.cell_w_px) as u16,
        px_h: (opts.rows as u32 * opts.cell_h_px) as u16,
    };

    info!(cols = opts.cols, rows = opts.rows, "spawning pty");
    let (pty, _child) = Pty::spawn(script.settings.shell.as_deref(), &script.env, pty_size)
        .context("spawning pty")?;

    let mut terminal = Terminal::new(TerminalOptions {
        cols: opts.cols,
        rows: opts.rows,
        max_scrollback: 1000,
    })?;
    terminal.resize(opts.cols, opts.rows, opts.cell_w_px, opts.cell_h_px)?;
    // Programs query terminal capabilities at startup. Without this hook,
    // those queries are dropped and applications such as vim/tmux can hang
    // waiting for a response.
    terminal.on_pty_write(|_t, data| pty.write(data))?;

    apply_theme(&mut terminal, &script.settings.theme)?;

    let mut translator = KeyTranslator::new()?;

    // Build absolute timeline.
    let (timeline, timeline_end) = build_timeline(&script.events, &script.settings);
    // The recording continues for one full frame interval after the last
    // event so the final state is always captured.
    let frame_interval = Duration::from_secs_f64(1.0 / script.settings.framerate as f64);
    // `total_duration` is mutable: it is extended when a `Wait` takes
    // longer than the pre-computed timeline so the recording window always
    // covers the full elapsed time plus the post-Wait tail.
    let mut total_duration = timeline_end + frame_interval * 4;

    // Expected wall-clock duration assuming `Wait` events resolve
    // instantly (i.e. just the sum of `Sleep` + typing/key delays).
    // This is the final timeline cursor from `build_timeline`, so it
    // includes trailing `Sleep` even when no event follows it. `Wait`
    // does not advance the cursor. Used by the decile-progress log lines
    // below so users can eyeball "we're 30 % through, ~12 s expected
    // total" while a tape is rendering.
    let expected_total = timeline_end;

    // Snapshot scratch state.
    let mut render_state = RenderState::new()?;
    let mut row_it = RowIterator::new()?;
    let mut cell_it = CellIterator::new()?;

    let start = Instant::now();
    let mut next_frame_at = Duration::ZERO;
    let mut event_idx = 0usize;
    let mut hidden = false;
    let mut hidden_started_at: Option<Duration> = None;
    let mut skipped_recording_time = Duration::ZERO;
    let mut clipboard = String::new();
    let mut pending_screenshots: Vec<PathBuf> = Vec::new();

    // Cursor-blink state: track when the cursor last changed screen position
    // so we can suppress blinking while (and briefly after) it is moving.
    // `None` = first frame not yet seen; `Some(None)` = cursor was hidden;
    // `Some(Some(pos))` = cursor was visible at `pos`.
    let mut last_cursor_moved_at: Option<Duration> = None;
    let mut prev_cursor_capture: Option<Option<(u16, u16)>> = None;

    // Wait‑for state. When we're inside a `Wait`, all later events stall
    // until the regex matches or the timeout elapses.
    let mut wait_state: Option<WaitState> = None;
    let mut expected_frames: u64 = 0;
    let mut captured_frames: u64 = 0;
    let mut max_raw_frame_consumer_queue_len: usize = 0;
    let mut raw_frame_consumer_dropped_frames: u64 = 0;

    // Decile progress tracking based on elapsed wall-clock time vs expected
    // timeline duration. We emit once when elapsed crosses each 10 % bucket.
    let total_actions = timeline.len();
    let mut next_decile: u32 = 10;
    info!(
        "timeline built: {total_actions} expanded actions, ~{expected:.1}s expected wall-clock (waits assumed instant)",
        expected = expected_total.as_secs_f64(),
    );

    loop {
        // 1. Drain everything currently available from the PTY.
        match pty.drain_into(&mut terminal) {
            Ok(()) => {}
            Err(PtyError::EndOfStream) => {
                debug!("pty closed");
                break;
            }
            Err(e) => return Err(anyhow::anyhow!(e)),
        }

        let now = start.elapsed();

        // 2. Resolve waits.
        if let Some(w) = &wait_state {
            if matches_wait(&terminal, w)? {
                // Extend the recording window by however long the Wait
                // actually took. Without this, `total_duration` (computed
                // before the run) would be in the past and the loop would
                // exit before the post-Wait tail (e.g. a `Sleep`) is shown.
                total_duration += now.saturating_sub(w.started_at);
                wait_state = None;
            } else if now >= w.deadline {
                warn!(pattern = %w.pattern, "wait timed out");
                total_duration += now.saturating_sub(w.started_at);
                wait_state = None;
            }
        }

        // 3. Advance through events whose deadline has passed.
        while wait_state.is_none() && event_idx < timeline.len() && timeline[event_idx].at <= now {
            let scheduled = &timeline[event_idx];
            event_idx += 1;
            trace!(
                event_idx,
                at_ms = scheduled.at.as_millis(),
                now_ms = now.as_millis(),
                event = ?scheduled.event,
                "dispatching scheduled event"
            );
            let was_hidden = hidden;
            execute_event(
                &scheduled.event,
                &pty,
                &mut translator,
                &terminal,
                &mut hidden,
                &mut wait_state,
                &mut clipboard,
                &mut pending_screenshots,
                start,
            )?;

            if !was_hidden && hidden {
                hidden_started_at = Some(now);
            }
            if was_hidden && !hidden {
                if let Some(hidden_start) = hidden_started_at.take() {
                    skipped_recording_time += now.saturating_sub(hidden_start);
                }
            }
        }

        // 3b. Decile progress logging. Emits one info line each time
        //     elapsed wall-clock crosses a multiple of 10 % of expected
        //     wall-clock duration.
        let expected_secs = expected_total.as_secs_f64();
        if expected_secs > 0.0 {
            let elapsed_pct = (now.as_secs_f64() * 100.0) / expected_secs;
            while next_decile <= 100 && elapsed_pct >= next_decile as f64 {
                info!(
                    "progress {pct}% ({elapsed:.1}s/{expected:.1}s expected, actions {done}/{total})",
                    pct = next_decile,
                    elapsed = now.as_secs_f64(),
                    expected = expected_secs,
                    done = event_idx,
                    total = total_actions,
                );
                next_decile += 10;
            }
        }

        // 4. Capture frames whose deadline has passed.
        while next_frame_at <= now && next_frame_at <= total_duration {
            if !hidden || !pending_screenshots.is_empty() {
                let recorded_at = next_frame_at.saturating_sub(skipped_recording_time);
                let (frame, raw_cursor_pos) = capture(
                    &mut render_state,
                    &mut row_it,
                    &mut cell_it,
                    &mut terminal,
                    // Compress timeline by subtracting wall-clock time spent
                    // hidden so rendered output doesn't stall across Hide/Show.
                    recorded_at,
                    opts.cols,
                    opts.rows,
                    script.settings.cursor_blink,
                    last_cursor_moved_at,
                )?;
                // Update cursor-movement tracking: any change in cursor
                // state (moved, appeared, or disappeared) counts as
                // "movement" and resets the blink-restart timer. The very
                // first captured frame is excluded from this comparison
                // because there is no prior frame to compare against.
                if let Some(prev) = prev_cursor_capture {
                    if raw_cursor_pos != prev {
                        last_cursor_moved_at = Some(recorded_at);
                    }
                }
                prev_cursor_capture = Some(raw_cursor_pos);
                if !pending_screenshots.is_empty() {
                    let shots = std::mem::take(&mut pending_screenshots);
                    for path in shots {
                        write_screenshot(&frame, script, &path)?;
                    }
                }
                if hidden {
                    next_frame_at += frame_interval;
                    continue;
                }
                expected_frames += 1;
                captured_frames += 1;
                for consumer in &raw_frame_consumers {
                    let consumer_len = consumer.len();
                    if consumer_len > max_raw_frame_consumer_queue_len {
                        max_raw_frame_consumer_queue_len = consumer_len;
                    }
                    match consumer.try_send(frame.clone()) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            raw_frame_consumer_dropped_frames += 1;
                        }
                        Err(TrySendError::Disconnected(_)) => {}
                    }
                }
            }
            next_frame_at += frame_interval;
        }

        // 5. Exit when both the script and the recording window are done.
        if event_idx >= timeline.len() && wait_state.is_none() && next_frame_at > total_duration {
            break;
        }

        // 6. Sleep until the next deadline, but wake up early if PTY data
        //    arrives.
        let next_deadline = compute_next_deadline(
            now,
            wait_state.as_ref(),
            timeline.get(event_idx),
            next_frame_at,
            total_duration,
        );
        if next_deadline > now {
            let timeout_ms = (next_deadline - now).as_millis().min(1000) as u16;
            let timeout = PollTimeout::try_from(timeout_ms).unwrap_or(PollTimeout::ZERO);
            let mut fds = [PollFd::new(
                unsafe { borrow_fd(pty.fd()) },
                PollFlags::POLLIN,
            )];
            let _ = poll(&mut fds, timeout);
        }
    }

    let raw_frame_consumer_count = raw_frame_consumers.len();
    drop(raw_frame_consumers);
    let stats = RunStats {
        expected_frames,
        captured_frames,
        raw_frame_consumer_count,
        max_raw_frame_consumer_queue_len,
        raw_frame_consumer_dropped_frames,
    };
    if raw_frame_consumer_dropped_frames > 0 {
        warn!(
            raw_frame_consumer_dropped_frames,
            "raw-frame consumer queue was full; dropped frames to keep terminal loop non-blocking"
        );
    }
    debug!(
        expected_frames,
        captured_frames,
        raw_frame_consumer_count,
        max_raw_frame_consumer_queue_len = stats.max_raw_frame_consumer_queue_len,
        raw_frame_consumer_dropped_frames = stats.raw_frame_consumer_dropped_frames,
        "pipeline stats"
    );
    info!("runner thread finished");
    Ok(stats)
}

// ---------------------------------------------------------------------------
// `Require` enforcement
// ---------------------------------------------------------------------------

/// Verify each `Require <prog>` directive resolves on `$PATH`. Bails with a
/// clear, actionable error listing every program that's missing. Mirrors
/// VHS's behaviour of failing fast before recording starts.
fn enforce_require(required: &[String]) -> Result<()> {
    if required.is_empty() {
        return Ok(());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&path).collect();
    let mut missing: Vec<&str> = Vec::new();
    for prog in required {
        if !is_program_on_path(prog, &dirs) {
            missing.push(prog.as_str());
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "the following `Require`d program(s) were not found on $PATH: {}. \
             Install them or remove the `Require` directive(s) from the tape.",
            missing.join(", ")
        );
    }
    info!(programs = ?required, "all `Require`d programs are present on PATH");
    Ok(())
}

fn is_program_on_path(prog: &str, dirs: &[std::path::PathBuf]) -> bool {
    // Treat anything containing a path separator as a literal path. Use
    // `std::path::is_separator` so both `/` and `\` count on Windows.
    let candidate = std::path::Path::new(prog);
    if candidate.is_absolute() || prog.chars().any(std::path::is_separator) {
        return std::fs::metadata(candidate).is_ok_and(|m| m.is_file());
    }
    for dir in dirs {
        let p = dir.join(prog);
        if std::fs::metadata(&p).is_ok_and(|m| m.is_file()) {
            return true;
        }
    }
    false
}

fn apply_theme(terminal: &mut Terminal<'_, '_>, theme: &crate::Theme) -> Result<()> {
    // OSC 4 controls indexed palette entries, OSC 10/11/12 control
    // foreground/background/cursor color. Using ST terminator keeps the
    // sequences unambiguous for the VT parser.
    for (idx, rgb) in theme.palette_rgb()?.iter().enumerate() {
        let seq = format!(
            "\x1b]4;{idx};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
            rgb[0], rgb[1], rgb[2]
        );
        terminal.vt_write(seq.as_bytes());
    }

    let fg_rgb = theme.foreground_rgb()?;
    let fg = format!(
        "\x1b]10;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        fg_rgb[0], fg_rgb[1], fg_rgb[2]
    );
    terminal.vt_write(fg.as_bytes());

    let bg_rgb = theme.background_rgb()?;
    let bg = format!(
        "\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        bg_rgb[0], bg_rgb[1], bg_rgb[2]
    );
    terminal.vt_write(bg.as_bytes());

    let cursor_rgb = theme.cursor_rgb()?;
    let cursor = format!(
        "\x1b]12;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        cursor_rgb[0], cursor_rgb[1], cursor_rgb[2]
    );
    terminal.vt_write(cursor.as_bytes());

    info!(theme = ?theme.name, "applied terminal theme");
    Ok(())
}

// ---------------------------------------------------------------------------
// Timeline construction
// ---------------------------------------------------------------------------

struct Scheduled {
    at: Duration,
    event: Event,
}

fn build_timeline(events: &[Event], settings: &Settings) -> (Vec<Scheduled>, Duration) {
    let mut out = Vec::new();
    let mut cursor = Duration::ZERO;
    let speed = settings.playback_speed.max(0.01);
    let scale = |d: Duration| Duration::from_secs_f64(d.as_secs_f64() / speed as f64);

    for ev in events {
        match ev {
            Event::Type { text, delay } => {
                let per = scale(*delay);
                // `Type` produces N character events per character so each
                // codepoint has its own deadline. We expand it here so the
                // runner just sends a single event at each tick.
                for (i, ch) in text.chars().enumerate() {
                    if i > 0 {
                        cursor += per;
                    }
                    out.push(Scheduled {
                        at: cursor,
                        event: Event::Type {
                            text: ch.to_string(),
                            // `delay` on the expanded events is unused.
                            delay: Duration::ZERO,
                        },
                    });
                }
                // Preserve the gap after the final typed character so the
                // next event (e.g. `Enter`) does not share the same timestamp.
                if !text.is_empty() {
                    cursor += per;
                }
            }
            Event::Sleep(d) => cursor += scale(*d),
            Event::Key {
                key,
                action,
                count,
                delay,
            } => {
                let per = scale(*delay);
                for i in 0..(*count).max(1) {
                    if i > 0 {
                        cursor += per;
                    }
                    out.push(Scheduled {
                        at: cursor,
                        event: Event::Key {
                            key: key.clone(),
                            action: *action,
                            count: 1,
                            delay: Duration::ZERO,
                        },
                    });
                }
            }
            Event::Wait { .. }
            | Event::Screenshot(_)
            | Event::Copy(_)
            | Event::Paste
            | Event::Hide
            | Event::Show => out.push(Scheduled {
                at: cursor,
                event: ev.clone(),
            }),
        }
    }
    (out, cursor)
}

// ---------------------------------------------------------------------------
// Event execution
// ---------------------------------------------------------------------------

struct WaitState {
    scope: WaitScope,
    pattern: String,
    deadline: Duration,
    /// Wall-clock time when this Wait was dispatched. Used to extend
    /// `total_duration` when the Wait resolves after the pre-computed
    /// timeline end, so frames captured during the Wait period remain
    /// visible in the output.
    started_at: Duration,
    re: Regex,
}

fn execute_event(
    event: &Event,
    pty: &Pty,
    translator: &mut KeyTranslator,
    terminal: &Terminal<'_, '_>,
    hidden: &mut bool,
    wait_state: &mut Option<WaitState>,
    clipboard: &mut String,
    pending_screenshots: &mut Vec<PathBuf>,
    start: Instant,
) -> Result<()> {
    match event {
        Event::Type { text, .. } => {
            // Each character is one expanded event – just send it.
            pty.write(text.as_bytes());
        }
        Event::Key { key, action, .. } => {
            warn_if_kitty_extended_required(key, *action, terminal)?;
            let bytes = translator.encode(key, *action, terminal)?;
            // The `Space` key shouldn't be sent through the encoder when
            // we're inside `Type` semantics, but for top‑level Space presses
            // a literal " " is the right thing.
            if !bytes.is_empty() {
                pty.write(bytes);
            } else if let NamedKey::Space = key.key {
                pty.write(b" ");
            }
        }
        Event::Sleep(_) => {
            // Sleep is materialised as a gap in the timeline; nothing to do
            // when we hit the (zero‑width) marker.
        }
        Event::Wait {
            scope,
            timeout,
            pattern,
        } => {
            let now = start.elapsed();
            let re =
                Regex::new(pattern).with_context(|| format!("invalid Wait regex `{pattern}`"))?;
            *wait_state = Some(WaitState {
                scope: *scope,
                pattern: pattern.clone(),
                deadline: now + *timeout,
                started_at: now,
                re,
            });
        }
        Event::Screenshot(path) => {
            pending_screenshots.push(resolve_output_path(path));
        }
        Event::Copy(text) => *clipboard = text.clone(),
        Event::Paste => pty.write(clipboard.as_bytes()),
        Event::Hide => *hidden = true,
        Event::Show => *hidden = false,
    }
    Ok(())
}

fn key_requires_kitty_extended(spec: &crate::script::KeySpec, action: KeyAction) -> bool {
    action == KeyAction::Release || spec.mods.super_key
}

fn warn_if_kitty_extended_required(
    spec: &crate::script::KeySpec,
    action: KeyAction,
    terminal: &Terminal<'_, '_>,
) -> Result<()> {
    if !key_requires_kitty_extended(spec, action) {
        return Ok(());
    }
    let kitty_flags = terminal.kitty_keyboard_flags()?;
    if kitty_flags.is_empty() || kitty_flags == KittyKeyFlags::DISABLED {
        warn!(
            ?spec,
            ?action,
            "key event likely requires kitty extended keyboard protocol, but kitty mode is disabled"
        );
    }
    Ok(())
}

fn resolve_output_path(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn write_screenshot(frame: &RawFrame, script: &Script, path: &std::path::Path) -> Result<()> {
    let render_opts = crate::RenderOptions {
        font_path: script.settings.font_family.clone(),
        font_size: script.settings.font_size,
        frame_style: FrameStyle {
            canvas_width_px: Some(script.settings.width),
            canvas_height_px: Some(script.settings.height),
            padding_px: script.settings.padding,
            margin_px: script.settings.margin,
            margin_fill: script.settings.margin_fill,
            window_bar: script.settings.window_bar,
            window_bar_size_px: script.settings.window_bar_size,
            border_radius_px: script.settings.border_radius,
        },
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating screenshot directory {}", parent.display()))?;
    }
    crate::render_gif::render_png_frame(frame, &render_opts, path)
        .with_context(|| format!("writing screenshot {}", path.display()))
}

fn matches_wait(term: &Terminal<'_, '_>, w: &WaitState) -> Result<bool> {
    let text = read_screen_text(term, w.scope)?;
    Ok(w.re.is_match(&text))
}

fn read_screen_text(term: &Terminal<'_, '_>, scope: WaitScope) -> Result<String> {
    // Use grid_ref for direct cell access. A temporary RenderState snapshot
    // would update the terminal's render-dirty tracking and cause the main
    // render_state (used in `capture`) to miss subsequent cell changes,
    // resulting in stale frames during long Wait periods.
    let rows = term.rows()? as u32;
    let cols = term.cols()? as u16;
    let mut last_line = String::new();
    let mut all = String::new();
    let mut buf = ['\0'; 8];

    for row in 0..rows {
        let mut line = String::new();
        for col in 0..cols {
            let gref = term.grid_ref(Point::Viewport(PointCoordinate { x: col, y: row }))?;
            match gref.graphemes(&mut buf) {
                Ok(0) => line.push(' '),
                Ok(n) => {
                    for &ch in &buf[..n] {
                        line.push(ch);
                    }
                }
                Err(_) => line.push(' '),
            }
        }
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            last_line = trimmed.to_string();
        }
        all.push_str(trimmed);
        all.push('\n');
    }
    Ok(match scope {
        WaitScope::Line => last_line,
        WaitScope::Screen => all,
    })
}

// ---------------------------------------------------------------------------
// Frame capture
// ---------------------------------------------------------------------------

fn capture<'a>(
    render_state: &mut RenderState<'a>,
    row_it: &mut RowIterator<'a>,
    cell_it: &mut CellIterator<'a>,
    terminal: &mut Terminal<'a, '_>,
    at: Duration,
    cols: u16,
    rows: u16,
    cursor_blink: bool,
    last_cursor_moved_at: Option<Duration>,
) -> Result<(RawFrame, Option<(u16, u16)>)> {
    let snap = render_state.update(terminal)?;
    let colors = snap.colors()?;
    let default_fg = rgb_to_arr(colors.foreground);
    let default_bg = rgb_to_arr(colors.background);

    let total = (cols as usize) * (rows as usize);
    let mut cells: Vec<CellSnap> = Vec::with_capacity(total);
    cells.resize_with(total, || CellSnap::blank(default_fg, default_bg));

    let mut row_iter = row_it.update(&snap)?;
    let mut row = 0u16;
    while let Some(rowit) = row_iter.next() {
        if row >= rows {
            break;
        }
        let mut cell_iter = cell_it.update(rowit)?;
        let mut col = 0u16;
        while let Some(cell) = cell_iter.next() {
            if col >= cols {
                break;
            }
            let idx = (row as usize) * (cols as usize) + (col as usize);
            let glen = cell.graphemes_len()?;
            let text = if glen > 0 {
                cell.graphemes()?.into_iter().collect::<String>()
            } else {
                String::new()
            };
            let fg = cell.fg_color()?.map(rgb_to_arr).unwrap_or(default_fg);
            let bg = cell.bg_color()?.map(rgb_to_arr).unwrap_or(default_bg);
            let style = cell.style()?;
            let mut flags = 0u8;
            if style.bold {
                flags |= style_flags::BOLD;
            }
            if style.italic {
                flags |= style_flags::ITALIC;
            }
            if style.inverse {
                flags |= style_flags::INVERSE;
            }
            if style.strikethrough {
                flags |= style_flags::STRIKETHROUGH;
            }
            // Underline is an enum (None/Single/Double/...) – treat anything
            // non‑None as a generic underline for now.
            if !matches!(style.underline, libghostty_vt::style::Underline::None) {
                flags |= style_flags::UNDERLINE;
            }
            cells[idx] = CellSnap {
                text,
                fg,
                bg,
                flags,
            };
            col += 1;
        }
        row += 1;
    }

    // Raw cursor position from the terminal (before blink logic).
    let raw_cursor_pos = if snap.cursor_visible()? {
        snap.cursor_viewport()?.map(|vp| (vp.x, vp.y))
    } else {
        None
    };

    let cursor = if let Some(pos) = raw_cursor_pos {
        if !cursor_blink {
            Some(pos)
        } else {
            match last_cursor_moved_at {
                None => {
                    // Cursor has not moved since recording started; use
                    // absolute-time blink so initial frames animate normally.
                    if cursor_visible_at(at) { Some(pos) } else { None }
                }
                Some(moved_at) => {
                    let time_since_move = at.saturating_sub(moved_at);
                    if time_since_move < CURSOR_BLINK_RESTART_DELAY {
                        // Cursor moved recently — keep it solid.
                        Some(pos)
                    } else {
                        // Cursor has been stationary long enough; blink from
                        // the moment it became stationary.
                        let blink_t = time_since_move - CURSOR_BLINK_RESTART_DELAY;
                        if cursor_visible_at(blink_t) { Some(pos) } else { None }
                    }
                }
            }
        }
    } else {
        None
    };

    Ok((
        RawFrame {
            t_ms: at.as_millis() as u32,
            cols,
            rows,
            cells,
            cursor,
            default_fg,
            default_bg,
        },
        raw_cursor_pos,
    ))
}

/// After the cursor has been stationary for this long, blinking resumes.
/// Equals one blink half-period (same value as `CURSOR_BLINK_HALF_PERIOD_MS`
/// inside `cursor_visible_at`), which is 0.5 × the full 600 ms blink cycle.
const CURSOR_BLINK_RESTART_DELAY: Duration = Duration::from_millis(300);

fn cursor_visible_at(at: Duration) -> bool {
    const CURSOR_BLINK_HALF_PERIOD_MS: u128 = 300;
    // Match a simple block cursor blink: 300ms visible, 300ms hidden.
    (at.as_millis() / CURSOR_BLINK_HALF_PERIOD_MS) % 2 == 0
}

fn rgb_to_arr(c: RgbColor) -> [u8; 3] {
    [c.r, c.g, c.b]
}

// ---------------------------------------------------------------------------
// Scheduling helpers
// ---------------------------------------------------------------------------

fn compute_next_deadline(
    now: Duration,
    wait: Option<&WaitState>,
    next_event: Option<&Scheduled>,
    next_frame: Duration,
    total: Duration,
) -> Duration {
    let mut next = next_frame.min(total + Duration::from_millis(1));
    if let Some(w) = wait {
        next = next.min(w.deadline);
    } else if let Some(ev) = next_event {
        next = next.min(ev.at);
    }
    next.max(now)
}

/// Borrow a raw fd as a `BorrowedFd` for use with `nix::poll`. The caller
/// guarantees the fd outlives the returned borrow.
unsafe fn borrow_fd(fd: std::os::fd::RawFd) -> std::os::fd::BorrowedFd<'static> {
    unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }
}

// Suppress unused field warnings on WaitState fields used only via Debug.
impl std::fmt::Debug for WaitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitState")
            .field("scope", &self.scope)
            .field("pattern", &self.pattern)
            .field("started_at", &self.started_at)
            .field("deadline", &self.deadline)
            .finish()
    }
}

// `_re` is used at runtime through `matches_wait`.
fn _silence_warnings(w: &WaitState) -> &Regex {
    &w.re
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use libghostty_vt::{Terminal, TerminalOptions};
    use tracing_subscriber::fmt::MakeWriter;

    use crate::script::{Event, KeySpec, ModSet, NamedKey, Settings};

    use super::{
        KeyAction, build_timeline, derive_options, key_requires_kitty_extended,
        warn_if_kitty_extended_required,
    };

    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedLogBuffer {
        fn snapshot(&self) -> String {
            String::from_utf8(self.0.lock().expect("lock log buffer").clone())
                .expect("utf8 log buffer")
        }
    }

    struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedLogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("lock log buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedLogBuffer {
        type Writer = SharedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedLogWriter(self.0.clone())
        }
    }

    #[test]
    fn default_settings_match_vhs_values() {
        let settings = Settings::default();

        assert_eq!(settings.font_size, 22.0);
        assert_eq!(settings.width, 1200);
        assert_eq!(settings.height, 600);
        assert_eq!(settings.padding, 60);
        assert_eq!(settings.framerate, 50);
    }

    #[test]
    fn vhs_defaults_produce_expected_layout() {
        let opts = derive_options(&Settings::default());

        assert_eq!(opts.cell_w_px, 11);
        assert_eq!(opts.cell_h_px, 22);
        assert_eq!(opts.cols, 98);
        assert_eq!(opts.rows, 21);
        assert_eq!(opts.frame_style.canvas_width_px, Some(1200));
        assert_eq!(opts.frame_style.canvas_height_px, Some(600));
    }

    #[test]
    fn detects_kitty_dependent_key_events() {
        let plain_press = KeySpec {
            key: NamedKey::Enter,
            mods: ModSet::NONE,
        };
        assert!(!key_requires_kitty_extended(&plain_press, KeyAction::Press));

        let release = KeySpec {
            key: NamedKey::Char('k'),
            mods: ModSet::NONE,
        };
        assert!(key_requires_kitty_extended(&release, KeyAction::Release));

        let super_mod = KeySpec {
            key: NamedKey::Right,
            mods: ModSet {
                super_key: true,
                ..ModSet::NONE
            },
        };
        assert!(key_requires_kitty_extended(&super_mod, KeyAction::Press));
    }

    #[test]
    fn emits_warning_for_kitty_dependent_keys_when_kitty_disabled() {
        let terminal = Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("terminal");

        let spec = KeySpec {
            key: NamedKey::Right,
            mods: ModSet {
                super_key: true,
                ..ModSet::NONE
            },
        };

        let logs = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .without_time()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            warn_if_kitty_extended_required(&spec, KeyAction::Press, &terminal)
                .expect("warn check");
        });

        assert!(
            logs.snapshot()
                .contains("key event likely requires kitty extended keyboard protocol"),
            "expected kitty warning to be emitted"
        );
    }

    #[test]
    fn type_preserves_gap_before_following_event() {
        let events = vec![
            Event::Type {
                text: "ab".to_string(),
                delay: Duration::from_millis(40),
            },
            Event::Key {
                key: KeySpec {
                    key: NamedKey::Enter,
                    mods: ModSet::NONE,
                },
                action: KeyAction::Press,
                count: 1,
                delay: Duration::from_millis(0),
            },
        ];

        let (timeline, end) = build_timeline(&events, &Settings::default());
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].at, Duration::from_millis(0));
        assert_eq!(timeline[1].at, Duration::from_millis(40));
        assert_eq!(timeline[2].at, Duration::from_millis(80));
        assert_eq!(end, Duration::from_millis(80));
    }
}

#[allow(unsafe_code)]
mod _unsafe_marker {
    // The `borrow_fd` helper above is `unsafe fn` so we tag the wrapper
    // module to keep `#![deny(unsafe_code)]` localised if we ever add it.
}
