use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use libghostty_vt::{
    Terminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
};
use ratatui::{
    Terminal as RatatuiTerminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tracing::info;
use unicode_width::UnicodeWidthChar;

use crate::keys::KeyTranslator;
use crate::pty::{Pty, PtySize};
use crate::recording::MouseState;
use crate::render_common::RenderOptions;
use crate::renderer;
use crate::runner::{apply_theme, capture, derive_options};
use crate::script::{
    Event, KeyAction, KeySpec, ModSet, MouseAction, MouseButton, NamedKey, Settings,
};
use crate::style::Theme;

/// Dynamic mouse coordinate encoder using libghostty's mouse event protocol
fn encode_mouse_event(
    action: MouseAction,
    button: Option<MouseButton>,
    col: u16,
    row: u16,
    terminal: &Terminal<'_, '_>,
    cell_width_px: u32,
    cell_height_px: u32,
    cols: u16,
    rows: u16,
) -> Result<Vec<u8>> {
    let x = (col as f32 * cell_width_px as f32) + (cell_width_px as f32 / 2.0);
    let y = (row as f32 * cell_height_px as f32) + (cell_height_px as f32 / 2.0);
    let pos = libghostty_vt::mouse::Position { x, y };

    let mut encoder = libghostty_vt::mouse::Encoder::new()?;
    encoder.set_options_from_terminal(terminal);

    let size = libghostty_vt::mouse::EncoderSize {
        screen_width: cols as u32 * cell_width_px,
        screen_height: rows as u32 * cell_height_px,
        cell_width: cell_width_px,
        cell_height: cell_height_px,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    };
    encoder.set_size(size);

    let any_pressed = match action {
        MouseAction::Press => true,
        MouseAction::Release => false,
        MouseAction::Motion => button.is_some(),
    };
    encoder.set_any_button_pressed(any_pressed);

    let mut mouse_event = libghostty_vt::mouse::Event::new()?;
    mouse_event.set_action(match action {
        MouseAction::Press => libghostty_vt::mouse::Action::Press,
        MouseAction::Release => libghostty_vt::mouse::Action::Release,
        MouseAction::Motion => libghostty_vt::mouse::Action::Motion,
    });
    mouse_event.set_button(button.map(|b| match b {
        MouseButton::Left => libghostty_vt::mouse::Button::Left,
        MouseButton::Right => libghostty_vt::mouse::Button::Right,
        MouseButton::Middle => libghostty_vt::mouse::Button::Middle,
        MouseButton::WheelUp => libghostty_vt::mouse::Button::Four,
        MouseButton::WheelDown => libghostty_vt::mouse::Button::Five,
    }));
    mouse_event.set_position(pos);

    let mut buf = vec![0u8; 64];
    let len = encoder.encode(&mouse_event, &mut buf)?;
    Ok(buf[..len].to_vec())
}

fn map_crossterm_key(event: crossterm::event::KeyEvent) -> (NamedKey, ModSet) {
    let key = match event.code {
        crossterm::event::KeyCode::Char(c) => NamedKey::Char(c),
        crossterm::event::KeyCode::Enter => NamedKey::Enter,
        crossterm::event::KeyCode::Tab => NamedKey::Tab,
        crossterm::event::KeyCode::Backspace => NamedKey::Backspace,
        crossterm::event::KeyCode::Delete => NamedKey::Delete,
        crossterm::event::KeyCode::Insert => NamedKey::Insert,
        crossterm::event::KeyCode::Esc => NamedKey::Escape,
        crossterm::event::KeyCode::Up => NamedKey::Up,
        crossterm::event::KeyCode::Down => NamedKey::Down,
        crossterm::event::KeyCode::Left => NamedKey::Left,
        crossterm::event::KeyCode::Right => NamedKey::Right,
        crossterm::event::KeyCode::PageUp => NamedKey::PageUp,
        crossterm::event::KeyCode::PageDown => NamedKey::PageDown,
        crossterm::event::KeyCode::Home => NamedKey::Home,
        crossterm::event::KeyCode::End => NamedKey::End,
        _ => NamedKey::Char(' '),
    };

    let mods = ModSet {
        ctrl: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL),
        alt: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::ALT),
        shift: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT),
        super_key: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::SUPER),
    };

    (key, mods)
}

fn map_crossterm_mouse(
    kind: crossterm::event::MouseEventKind,
) -> Option<(MouseAction, Option<MouseButton>)> {
    match kind {
        crossterm::event::MouseEventKind::Down(btn) => {
            Some((MouseAction::Press, map_mouse_button(btn)))
        }
        crossterm::event::MouseEventKind::Up(btn) => {
            Some((MouseAction::Release, map_mouse_button(btn)))
        }
        crossterm::event::MouseEventKind::Drag(btn) => {
            Some((MouseAction::Motion, map_mouse_button(btn)))
        }
        crossterm::event::MouseEventKind::Moved => Some((MouseAction::Motion, None)),
        crossterm::event::MouseEventKind::ScrollUp => {
            Some((MouseAction::Press, Some(MouseButton::WheelUp)))
        }
        crossterm::event::MouseEventKind::ScrollDown => {
            Some((MouseAction::Press, Some(MouseButton::WheelDown)))
        }
        crossterm::event::MouseEventKind::ScrollLeft
        | crossterm::event::MouseEventKind::ScrollRight => None,
    }
}

fn map_mouse_button(btn: crossterm::event::MouseButton) -> Option<MouseButton> {
    match btn {
        crossterm::event::MouseButton::Left => Some(MouseButton::Left),
        crossterm::event::MouseButton::Right => Some(MouseButton::Right),
        crossterm::event::MouseButton::Middle => Some(MouseButton::Middle),
    }
}

fn is_simple_char(key: NamedKey, mods: ModSet) -> bool {
    matches!(key, NamedKey::Char(_)) && !mods.ctrl && !mods.alt && !mods.super_key
}

fn flush_char_buffer(
    char_buffer: &mut String,
    recorded_events: &mut Vec<Event>,
    last_event_time: &mut Instant,
) {
    if !char_buffer.is_empty() {
        recorded_events.push(Event::Type {
            text: char_buffer.clone(),
            delay: Duration::from_millis(50),
        });
        char_buffer.clear();
        *last_event_time = Instant::now();
    }
}

/// Renders the host terminal state utilizing Ratatui's canvas drawing and text formatting backend
struct HeaderLayoutResult {
    height: u16,
    click_cells: Vec<(u16, u16)>,
}

fn layout_header(
    mut buf: Option<&mut ratatui::buffer::Buffer>,
    area: ratatui::layout::Rect,
    elapsed: Duration,
    show_dot: bool,
    is_hovered: bool,
) -> HeaderLayoutResult {
    let mut click_cells = Vec::new();
    let mut x = area.x;
    let mut y = area.y;
    let width = area.width;

    if width == 0 {
        return HeaderLayoutResult {
            height: 0,
            click_cells,
        };
    }

    let dot_style = Style::default().fg(Color::Green);
    let normal_style = Style::default();
    let mut click_style = Style::default().add_modifier(Modifier::UNDERLINED);
    if is_hovered {
        click_style = click_style.add_modifier(Modifier::REVERSED);
    }

    let dot_text = if show_dot { "●" } else { " " };
    let seconds_str = format!("{}s", elapsed.as_secs());
    let prefix_text = format!(
        " EVP recording active ({}). To stop recording, exit the program or ",
        seconds_str
    );
    let click_text = "click here";
    let suffix_text = ".";

    let parts = vec![
        (dot_text.to_string(), dot_style, false),
        (prefix_text, normal_style, false),
        (click_text.to_string(), click_style, true),
        (suffix_text.to_string(), normal_style, false),
    ];

    for (text, style, is_click) in parts {
        for c in text.chars() {
            if x >= area.x + width {
                x = area.x;
                y += 1;
            }
            if let Some(ref mut b) = buf {
                if y < area.y + area.height {
                    let cell = &mut b[(x, y)];
                    cell.set_char(c);
                    cell.set_style(style);
                }
            }
            if is_click {
                click_cells.push((x, y));
            }
            x += 1;
        }
    }

    HeaderLayoutResult {
        height: (y - area.y) + 1,
        click_cells,
    }
}

fn draw_terminal_state(
    ratatui_term: &mut RatatuiTerminal<CrosstermBackend<std::io::Stdout>>,
    frame: &crate::recording::RawFrame,
    elapsed: Duration,
    host_mouse_pos: Option<(u16, u16)>,
) -> Result<(u16, Vec<(u16, u16)>)> {
    let mut click_cells = Vec::new();
    let mut header_height = 1u16;

    ratatui_term.draw(|f| {
        let show_dot = (elapsed.as_millis() % 1000) < 500;

        // 1. Dry run to calculate header height and hover state
        let is_hovered = if let Some((m_col, m_row)) = host_mouse_pos {
            let dry_run = layout_header(None, f.area(), elapsed, show_dot, false);
            dry_run.click_cells.contains(&(m_col, m_row))
        } else {
            false
        };

        let dry_run = layout_header(None, f.area(), elapsed, show_dot, is_hovered);
        header_height = dry_run.height;

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height), // Dynamic Header
                Constraint::Length(1),             // Divider
                Constraint::Min(0),                // Terminal body
            ])
            .split(f.area());

        // 2. Real draw run
        let layout_res = layout_header(
            Some(f.buffer_mut()),
            chunks[0],
            elapsed,
            show_dot,
            is_hovered,
        );
        click_cells = layout_res.click_cells;

        // 3. Divider widget
        let divider_line = "─".repeat(chunks[1].width as usize);
        f.render_widget(Paragraph::new(divider_line), chunks[1]);

        // 4. Render terminal rows from grid cells
        let mut lines = Vec::with_capacity(frame.rows as usize);
        let cols = frame.cols as usize;

        for r in 0..(frame.rows as usize) {
            let mut spans = Vec::with_capacity(cols);
            let mut prev_is_wide = false;

            for c in 0..cols {
                let idx = r * cols + c;
                if idx >= frame.cells.len() {
                    break;
                }
                let cell = &frame.cells[idx];

                if prev_is_wide {
                    prev_is_wide = false;
                    continue;
                }

                prev_is_wide = cell
                    .text
                    .chars()
                    .next()
                    .map(|ch| ch.width() == Some(2))
                    .unwrap_or(false);

                let mut style = Style::default()
                    .fg(Color::Rgb(cell.fg[0], cell.fg[1], cell.fg[2]))
                    .bg(Color::Rgb(cell.bg[0], cell.bg[1], cell.bg[2]));

                if cell.flags & 1 != 0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.flags & 2 != 0 {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.flags & 4 != 0 {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.flags & 8 != 0 {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if cell.flags & 16 != 0 {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                if cell.flags & 32 != 0 {
                    style = style.add_modifier(Modifier::DIM);
                }

                let text = if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                };
                spans.push(Span::styled(text.to_string(), style));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), chunks[2]);

        // 5. Update hardware cursor coordinate
        if let Some((ccol, crow)) = frame.cursor {
            if ccol < frame.cols && crow < frame.rows {
                f.set_cursor_position((ccol, crow + header_height + 1));
            }
        }
    })?;

    Ok((header_height, click_cells))
}

/// Distance from point C to line AB. Collinear if within deviation threshold.
fn is_collinear(ax: u16, ay: u16, bx: u16, by: u16, cx: u16, cy: u16) -> bool {
    let dx = bx as f32 - ax as f32;
    let dy = by as f32 - ay as f32;

    let length_sq = dx * dx + dy * dy;
    if length_sq < 0.01 {
        return true;
    }

    let numerator = ((dy * cx as f32) - (dx * cy as f32) + (bx as f32 * ay as f32)
        - (by as f32 * ax as f32))
        .abs();
    let distance = numerator / length_sq.sqrt();

    distance <= 1.5
}

/// Accumulates a continuous sequence of mouse movements and flushes a single simplified
/// MouseMove or MouseDrag event when collinearity is broken or a pause of >1s occurs.
struct MouseSegmentTracker {
    points: Vec<(u16, u16, Instant)>,
    is_drag: bool,
}

impl MouseSegmentTracker {
    fn new(is_drag: bool) -> Self {
        Self {
            points: Vec::new(),
            is_drag,
        }
    }

    fn add_point(&mut self, col: u16, row: u16, now: Instant) -> Option<Event> {
        if self.points.is_empty() {
            self.points.push((col, row, now));
            return None;
        }

        let last_point = *self.points.last().unwrap();

        // 1. If paused for more than 1s, break to a new segment
        if now.duration_since(last_point.2) > Duration::from_secs(1) {
            let ev = self.flush();
            self.points.push((col, row, now));
            return ev;
        }

        // 2. Ignore duplicate points
        if last_point.0 == col && last_point.1 == row {
            self.points.pop();
            self.points.push((col, row, now));
            return None;
        }

        // 3. Collinearity validation
        if self.points.len() >= 2 {
            let start = self.points[0];
            let end = (col, row);

            for &p in &self.points[1..] {
                if !is_collinear(start.0, start.1, end.0, end.1, p.0, p.1) {
                    let ev = self.flush();
                    self.points.push(last_point);
                    self.points.push((col, row, now));
                    return ev;
                }
            }
        }

        self.points.push((col, row, now));
        None
    }

    fn flush(&mut self) -> Option<Event> {
        if self.points.len() < 2 {
            self.points.clear();
            return None;
        }

        let start = self.points.first().unwrap();
        let end = self.points.last().unwrap();
        let duration = end.2.duration_since(start.2);
        let delay = if duration < Duration::from_millis(50) {
            Duration::from_millis(50)
        } else {
            duration
        };

        let ev = if self.is_drag {
            Some(Event::MouseDrag {
                start_col: start.0,
                start_row: start.1,
                end_col: end.0,
                end_row: end.1,
                delay,
                easing: None,
            })
        } else {
            Some(Event::MouseMove {
                start_col: start.0,
                start_row: start.1,
                end_col: end.0,
                end_row: end.1,
                delay,
                easing: None,
            })
        };

        self.points.clear();
        ev
    }
}

struct TerminalCapabilityGuard;

impl TerminalCapabilityGuard {
    fn new() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("enabling terminal raw mode")?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableBracketedPaste,
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )
        .context("initializing host terminal capabilities")?;
        Ok(Self)
    }
}

impl Drop for TerminalCapabilityGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags,
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Interactive PTY multiplexer that records keystrokes to a `.tape` file and encodes raw frames
/// in real-time to a background GIF compiler.
pub fn record(
    tape_path: PathBuf,
    shell: Option<String>,
    override_cols: Option<u16>,
    override_rows: Option<u16>,
    theme_name: Option<String>,
    output_override: Option<PathBuf>,
) -> Result<()> {
    // 1. Resolve geometry
    let (actual_host_cols, actual_host_rows) =
        crossterm::terminal::size().context("getting host terminal size")?;

    let mut cols = override_cols.unwrap_or(actual_host_cols);
    let mut rows = override_rows.unwrap_or(actual_host_rows.saturating_sub(2));

    if rows == 0 {
        bail!("terminal height is too small for EVP recording");
    }

    let mut settings = Settings::default();
    settings.cols = Some(cols);
    settings.rows = Some(rows);
    if let Some(sh) = &shell {
        settings.shell = Some(sh.clone());
    }
    if let Some(t_name) = &theme_name {
        if let Ok(theme) = Theme::from_spec(t_name) {
            settings.theme = theme;
        }
    }

    let cfg = derive_options(&settings);
    let pty_size = PtySize {
        cols,
        rows,
        px_w: cols * cfg.cell_width_px as u16,
        px_h: rows * cfg.cell_height_px as u16,
    };

    // 2. Spawn PTY and child process shell
    info!(cols, rows, ?shell, "starting interactive recording");
    let (pty, mut child) = Pty::spawn(shell.as_deref(), &[], pty_size, false)
        .context("spawning PTY for record session")?;

    // 3. Initialize libghostty VT
    let mut terminal = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 1000,
    })?;
    terminal.resize(cols, rows, cfg.cell_width_px, cfg.cell_height_px)?;
    terminal.on_pty_write(|_t, data| pty.write(data))?;

    apply_theme(&mut terminal, &settings.theme)?;

    let mut translator = KeyTranslator::new()?;

    // 4. Initialize background GIF/SVG/JSON renderer
    let out_path = output_override.unwrap_or_else(|| tape_path.with_extension("gif"));
    let render_opts = RenderOptions {
        font_path: settings.font_family.clone(),
        font_size: settings.font_size,
        line_height: settings.line_height,
        letter_spacing: settings.letter_spacing,
        frame_style: cfg.frame_style.clone(),
        no_system_fonts: false,
    };
    let backend = renderer::RendererBackend::for_path(&out_path, &render_opts, true, false)?;

    let renderer_handle = renderer::spawn_renderer(cfg, backend, out_path.clone())
        .context("spawning background renderer")?;
    let renderer_tx = renderer_handle.tx.clone();

    // 5. Setup channels and multi-threaded event polling
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        loop {
            match crossterm::event::read() {
                Ok(event) => {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let pty_read_clone = pty.try_clone().context("cloning pty for read thread")?;
    let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        use nix::poll::{PollFd, PollFlags};
        let mut poll_fds = [PollFd::new(pty_read_clone.as_fd(), PollFlags::POLLIN)];
        let mut buf = [0u8; 8192];
        loop {
            match nix::poll::poll(&mut poll_fds, Option::<u16>::None) {
                Ok(0) => continue,
                Ok(_) => match nix::unistd::read(&pty_read_clone.as_fd(), &mut buf) {
                    Ok(0) => {
                        let _ = pty_tx.send(vec![]);
                        break;
                    }
                    Ok(n) => {
                        if pty_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => {
                        let _ = pty_tx.send(vec![]);
                        break;
                    }
                },
                Err(_) => {
                    let _ = pty_tx.send(vec![]);
                    break;
                }
            }
        }
    });

    let ticker = crossbeam_channel::tick(Duration::from_millis(1000 / settings.framerate as u64));

    // 7. Interactive Loop
    let mut recorded_events = Vec::new();
    let start_time = Instant::now();
    let mut last_event_time = start_time;
    let mut char_buffer = String::new();
    let mut active_tracker: Option<MouseSegmentTracker> = None;

    {
        // 6. Enter alternate screen buffer, raw mode, and enable host terminal capabilities
        let _guard = TerminalCapabilityGuard::new()?;

        let backend = CrosstermBackend::new(std::io::stdout());
        let mut ratatui_term = RatatuiTerminal::new(backend)?;
        ratatui_term.clear()?;

        // Mouse tracking variables
        let mut current_mouse_col = 0u16;
        let mut current_mouse_row = 0u16;
        let mut is_dragging = false;
        let mut drag_start_col = 0u16;
        let mut drag_start_row = 0u16;
        let mut current_mouse_pos: Option<(f32, f32, MouseState)> = None;
        let mut last_mouse_move_time = start_time;
        let mut host_mouse_pos: Option<(u16, u16)> = None;
        let mut click_here_cells: Vec<(u16, u16)> = Vec::new();
        let mut header_height = 1u16;

        let mut render_state = RenderState::new()?;
        let mut row_it = RowIterator::new()?;
        let mut cell_it = CellIterator::new()?;

        loop {
            crossbeam_channel::select! {
                recv(pty_rx) -> res => {
                    match res {
                        Ok(data) => {
                            if data.is_empty() {
                                break; // EOF
                            }
                            // Feed output to libghostty VT parser
                            terminal.vt_write(&data);
                        }
                        Err(_) => break,
                    }
                }
                recv(event_rx) -> res => {
                    match res {
                        Ok(event) => {
                            match event {
                                crossterm::event::Event::Key(key_event) => {
                                    // Interrupt and flush active mouse movement
                                    if let Some(mut tracker) = active_tracker.take() {
                                        if let Some(ev) = tracker.flush() {
                                            recorded_events.push(ev);
                                        }
                                    }

                                    let (named_key, mods) = map_crossterm_key(key_event);
                                    let key_spec = KeySpec { key: named_key, mods };
                                    let action = if key_event.kind == crossterm::event::KeyEventKind::Release {
                                        KeyAction::Release
                                    } else {
                                        KeyAction::Press
                                    };

                                    let now = Instant::now();

                                    if action == KeyAction::Press {
                                        let idle_duration = now.duration_since(last_event_time);

                                        // Flush buffer on significant idle or special keys
                                        if idle_duration > Duration::from_secs(1) || !is_simple_char(named_key, mods) {
                                            flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);
                                        }

                                        // Output sleep if we flushed/idle
                                        let current_idle = now.duration_since(last_event_time);
                                        if current_idle > Duration::from_millis(50) && char_buffer.is_empty() {
                                            recorded_events.push(Event::Sleep(current_idle));
                                            last_event_time = now;
                                        }

                                        if is_simple_char(named_key, mods) {
                                            if let NamedKey::Char(c) = named_key {
                                                char_buffer.push(c);
                                            }
                                        } else {
                                            recorded_events.push(Event::Key {
                                                key: key_spec.clone(),
                                                action: KeyAction::Press,
                                                count: 1,
                                                delay: Duration::from_millis(50),
                                            });
                                            last_event_time = now;
                                        }
                                    }

                                    if let Ok(bytes) = translator.encode(&key_spec, action, &terminal) {
                                        pty.write(bytes);
                                    }
                                }
                                crossterm::event::Event::Mouse(mouse_event) => {
                                    host_mouse_pos = Some((mouse_event.column, mouse_event.row));

                                    let is_click_here = click_here_cells.contains(&(mouse_event.column, mouse_event.row));

                                    if is_click_here {
                                        if let crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse_event.kind {
                                            if let crate::pty::Child::Active(pid) = child {
                                                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGHUP);
                                                std::thread::sleep(std::time::Duration::from_millis(100));
                                                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                                                let _ = nix::sys::wait::waitpid(pid, None);
                                                child = crate::pty::Child::Reaped;
                                            }
                                            break;
                                        }

                                        // Redraw to update hover style instantly
                                        let elapsed = start_time.elapsed();
                                        if let Ok((mut frame, _)) = capture(
                                            &mut render_state,
                                            &mut row_it,
                                            &mut cell_it,
                                            &mut terminal,
                                            elapsed,
                                            cols,
                                            rows,
                                            true,
                                            None,
                                            None,
                                        ) {
                                            frame.mouse_cursor = current_mouse_pos;
                                            if let Ok((height, cells)) = draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos) {
                                                header_height = height;
                                                click_here_cells = cells;
                                            }
                                        }
                                        continue;
                                    }

                                    // If we moved off "click here" to another part of the header/divider, redraw to clear hover
                                    if mouse_event.row < header_height + 1 {
                                        let elapsed = start_time.elapsed();
                                        if let Ok((mut frame, _)) = capture(
                                            &mut render_state,
                                            &mut row_it,
                                            &mut cell_it,
                                            &mut terminal,
                                            elapsed,
                                            cols,
                                            rows,
                                            true,
                                            None,
                                            None,
                                        ) {
                                            frame.mouse_cursor = current_mouse_pos;
                                            if let Ok((height, cells)) = draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos) {
                                                header_height = height;
                                                click_here_cells = cells;
                                            }
                                        }
                                        continue;
                                    }

                                    if let Some((action, button)) = map_crossterm_mouse(mouse_event.kind) {
                                            let col = mouse_event.column;
                                            let row = mouse_event.row;

                                            // Translate row coordinate: subtract header rows and divider row
                                            let row = row - (header_height + 1);

                                            if col >= cols || row >= rows {
                                                continue;
                                            }

                                            let now = Instant::now();
                                            last_mouse_move_time = now;
                                            flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);

                                            // Update local dragging flag
                                            match action {
                                                MouseAction::Press => {
                                                    if button == Some(MouseButton::Left) {
                                                        is_dragging = true;
                                                    }
                                                }
                                                MouseAction::Release => {
                                                    if button == Some(MouseButton::Left) {
                                                        is_dragging = false;
                                                    }
                                                }
                                                _ => {}
                                            }

                                            // Update pointer coordinates for live rendering
                                            let mouse_state = if is_dragging {
                                                MouseState::Dragging
                                            } else if action == MouseAction::Press {
                                                MouseState::Clicking
                                            } else {
                                                MouseState::Moving
                                            };
                                            current_mouse_pos = Some((col as f32, row as f32, mouse_state));

                                            // 1. If it's movement (Motion / Drag), feed to segment tracker
                                            let is_drag = action == MouseAction::Motion && is_dragging;
                                            let is_move = action == MouseAction::Motion && !is_dragging;

                                            if is_drag || is_move {
                                                let mut start_new = false;
                                                if let Some(ref tracker) = active_tracker {
                                                    if tracker.is_drag != is_drag {
                                                        if let Some(ev) = active_tracker.as_mut().unwrap().flush() {
                                                            recorded_events.push(ev);
                                                        }
                                                        start_new = true;
                                                    }
                                                } else {
                                                    start_new = true;
                                                }

                                                if start_new {
                                                    let mut tracker = MouseSegmentTracker::new(is_drag);
                                                    // Initialize segment starting point at the previous mouse coords
                                                    tracker.points.push((current_mouse_col, current_mouse_row, last_event_time));
                                                    active_tracker = Some(tracker);
                                                }

                                                if let Some(ref mut tracker) = active_tracker {
                                                    if let Some(ev) = tracker.add_point(col, row, now) {
                                                        recorded_events.push(ev);
                                                    }
                                                }
                                                current_mouse_col = col;
                                                current_mouse_row = row;
                                                last_event_time = now;
                                            } else {
                                                // 2. Otherwise (Press / Release / Scroll), flush movement segment first
                                                if let Some(mut tracker) = active_tracker.take() {
                                                    if let Some(ev) = tracker.flush() {
                                                        recorded_events.push(ev);
                                                    }
                                                }

                                                let current_idle = now.duration_since(last_event_time);
                                                if current_idle > Duration::from_millis(50) {
                                                    recorded_events.push(Event::Sleep(current_idle));
                                                }

                                                // Process instantaneous mouse event
                                                match action {
                                                    MouseAction::Press => {
                                                        if button == Some(MouseButton::Left) {
                                                            drag_start_col = col;
                                                            drag_start_row = row;
                                                        }
                                                    }
                                                    MouseAction::Release => {
                                                        if button == Some(MouseButton::Left) {
                                                            if col == drag_start_col && row == drag_start_row {
                                                                recorded_events.push(Event::Click {
                                                                    col,
                                                                    row,
                                                                    delay: Duration::from_millis(50),
                                                                });
                                                            } else {
                                                                recorded_events.push(Event::MouseDrag {
                                                                    start_col: drag_start_col,
                                                                    start_row: drag_start_row,
                                                                    end_col: col,
                                                                    end_row: row,
                                                                    delay: Duration::from_millis(50),
                                                                    easing: None,
                                                                });
                                                            }
                                                        } else if button == Some(MouseButton::Right) {
                                                            recorded_events.push(Event::RightClick {
                                                                col,
                                                                row,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        } else if button == Some(MouseButton::WheelUp) {
                                                            recorded_events.push(Event::MouseScroll {
                                                                col,
                                                                row,
                                                                direction: crate::script::ScrollDirection::Up,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        } else if button == Some(MouseButton::WheelDown) {
                                                            recorded_events.push(Event::MouseScroll {
                                                                col,
                                                                row,
                                                                direction: crate::script::ScrollDirection::Down,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        }
                                                        current_mouse_col = col;
                                                        current_mouse_row = row;
                                                    }
                                                    _ => {}
                                                }
                                                last_event_time = now;
                                            }

                                            // Encode and transmit mouse coordinates to the PTY
                                            if let Ok(bytes) = encode_mouse_event(
                                                action,
                                                button,
                                                col,
                                                row,
                                                &terminal,
                                                cfg.cell_width_px,
                                                cfg.cell_height_px,
                                                cols,
                                                rows,
                                            ) {
                                                pty.write(&bytes);
                                            }
                                        }
                                    }
                                crossterm::event::Event::Paste(text) => {
                                    flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);
                                    let now = Instant::now();
                                    let idle_duration = now.duration_since(last_event_time);
                                    if idle_duration > Duration::from_millis(50) {
                                        recorded_events.push(Event::Sleep(idle_duration));
                                    }
                                    recorded_events.push(Event::Type {
                                        text: text.clone(),
                                        delay: Duration::from_millis(50),
                                    });
                                    last_event_time = now;
                                    pty.write(text.as_bytes());
                                }
                                crossterm::event::Event::Resize(new_host_cols, new_host_rows) => {
                                    let new_cols = new_host_cols;
                                    let new_rows = new_host_rows.saturating_sub(2);
                                    if new_rows > 0 {
                                        cols = new_cols;
                                        rows = new_rows;
                                        let _ = terminal.resize(new_cols, new_rows, cfg.cell_width_px, cfg.cell_height_px);
                                        let pty_size = PtySize {
                                            cols: new_cols,
                                            rows: new_rows,
                                            px_w: new_cols * cfg.cell_width_px as u16,
                                            px_h: new_rows * cfg.cell_height_px as u16,
                                        };
                                        pty.resize(pty_size);
                                        let _ = ratatui_term.resize(ratatui::prelude::Rect::new(0, 0, new_host_cols, new_host_rows));
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(_) => break,
                    }
                }
                recv(ticker) -> _ => {
                    let elapsed = start_time.elapsed();
                    if let Ok((mut frame, _)) = capture(
                        &mut render_state,
                        &mut row_it,
                        &mut cell_it,
                        &mut terminal,
                        elapsed,
                        cols,
                        rows,
                        true,
                        None,
                        None,
                    ) {
                        if Instant::now().duration_since(last_mouse_move_time) > Duration::from_secs(3) {
                            current_mouse_pos = None;
                        }
                        // Inject mouse pointer into the frame so the GIF compiler renders it
                        frame.mouse_cursor = current_mouse_pos;

                        // Update host screen via Ratatui
                        if let Ok((height, cells)) = draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos) {
                            header_height = height;
                            click_here_cells = cells;
                        }

                        // Send to background renderer
                        let _ = renderer_tx.try_send(frame);
                    }
                }
            }
        }
    }

    info!("interactive session finished; compiling recording in background...");

    // Flush any remaining active movements or buffered characters
    if let Some(mut tracker) = active_tracker.take() {
        if let Some(ev) = tracker.flush() {
            recorded_events.push(ev);
        }
    }
    flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);

    // 9. Format and write `.tape` file
    let tape_content = crate::script::write_tape_script(
        &recorded_events,
        &out_path,
        shell.as_deref(),
        cols,
        rows,
        theme_name.as_deref(),
    );

    std::fs::write(&tape_path, tape_content)
        .with_context(|| format!("writing tape file to {}", tape_path.display()))?;

    // Drop the renderer tx to signal EOF, then await completion
    drop(renderer_tx);
    renderer_handle
        .join()
        .context("awaiting background GIF renderer compilation")?;

    let _ = child;
    info!(
        tape = %tape_path.display(),
        output = %out_path.display(),
        "recording completed successfully"
    );
    Ok(())
}
