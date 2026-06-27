use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use libghostty_vt::{
    Terminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal as RatatuiTerminal,
};
use tracing::info;
use unicode_width::UnicodeWidthChar;

use crate::keys::KeyTranslator;
use crate::pty::{Pty, PtySize};
use crate::render_common::RenderOptions;
use crate::renderer;
use crate::runner::{apply_theme, capture, derive_options};
use crate::script::{
    Event, KeyAction, KeySpec, ModSet, MouseAction, MouseButton, NamedKey,
    Settings,
};
use crate::style::Theme;

const REF_SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/ref_script.tape"));

/// Helper class to scan PTY output buffer for mouse mode transitions
struct MouseModeScanner {
    history: Vec<u8>,
}

impl MouseModeScanner {
    fn new() -> Self {
        Self {
            history: Vec::with_capacity(128),
        }
    }

    fn scan(&mut self, data: &[u8]) -> Option<bool> {
        self.history.extend_from_slice(data);
        if self.history.len() > 128 {
            let start = self.history.len() - 128;
            self.history.drain(..start);
        }
        let s = String::from_utf8_lossy(&self.history);

        let mut enabled = None;
        let mut max_idx = 0;

        let patterns = [
            ("?1000h", true),
            ("?1002h", true),
            ("?1003h", true),
            ("?1006h", true),
            ("?1000l", false),
            ("?1002l", false),
            ("?1003l", false),
            ("?1006l", false),
        ];

        for (pattern, val) in patterns {
            if let Some(idx) = s.rfind(pattern) {
                if idx >= max_idx {
                    max_idx = idx;
                    enabled = Some(val);
                }
            }
        }
        enabled
    }
}

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
        alt: event.modifiers.contains(crossterm::event::KeyModifiers::ALT),
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
    matches!(key, NamedKey::Char(_))
        && !mods.ctrl
        && !mods.alt
        && !mods.super_key
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
fn draw_terminal_state(
    ratatui_term: &mut RatatuiTerminal<CrosstermBackend<std::io::Stdout>>,
    frame: &crate::recording::RawFrame,
    elapsed: Duration,
) -> Result<()> {
    ratatui_term.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(1), // Divider
                Constraint::Min(0),    // Terminal body
            ])
            .split(f.area());

        // 1. Header widget with blinking green dot (500ms on, 500ms off)
        let show_dot = (elapsed.as_millis() % 1000) < 500;
        let blink_dot = if show_dot {
            Span::styled("●", Style::default().fg(Color::Green))
        } else {
            Span::raw(" ")
        };
        let info_text = Span::raw(format!(
            " EVP recording active ({}s), exit the program to stop recording.",
            elapsed.as_secs()
        ));
        f.render_widget(Paragraph::new(Line::from(vec![blink_dot, info_text])), chunks[0]);

        // 2. Divider widget
        let divider_line = "─".repeat(chunks[1].width as usize);
        f.render_widget(Paragraph::new(divider_line), chunks[1]);

        // 3. Render terminal rows from grid cells
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

                let text = if cell.text.is_empty() { " " } else { &cell.text };
                spans.push(Span::styled(text.to_string(), style));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), chunks[2]);

        // 4. Update hardware cursor coordinate
        if let Some((ccol, crow)) = frame.cursor {
            if ccol < frame.cols && crow < frame.rows {
                f.set_cursor_position((ccol, crow + 2));
            }
        }
    })?;
    Ok(())
}

/// Interactive PTY multiplexer that records keystrokes to a `.tape` file and encodes raw frames
/// in real-time to a background GIF compiler.
pub fn record(
    tape_path: PathBuf,
    shell: Option<String>,
    override_cols: Option<u16>,
    override_rows: Option<u16>,
    theme_name: Option<String>,
) -> Result<()> {
    // 1. Resolve geometry
    let (actual_host_cols, actual_host_rows) = crossterm::terminal::size()
        .context("getting host terminal size")?;

    let cols = override_cols.unwrap_or(actual_host_cols);
    let rows = override_rows.unwrap_or(actual_host_rows.saturating_sub(2));

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
    let (pty, _child) = Pty::spawn(
        shell.as_deref(),
        &[],
        pty_size,
        false,
    )
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

    // 4. Initialize background GIF/SVG renderer
    let gif_path = tape_path.with_extension("gif");
    let render_opts = RenderOptions {
        font_path: settings.font_family.clone(),
        font_size: settings.font_size,
        line_height: settings.line_height,
        letter_spacing: settings.letter_spacing,
        frame_style: cfg.frame_style.clone(),
        no_system_fonts: false,
    };
    let backend = renderer::RendererBackend::Gif(render_opts.clone());
    let renderer_handle = renderer::spawn_renderer(cfg, backend, gif_path.clone())
        .context("spawning background GIF renderer")?;
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
                    Err(nix::errno::Errno::EAGAIN)
                    | Err(nix::errno::Errno::EINTR) => continue,
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

    // 6. Enter alternate screen buffer and raw mode
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)
        .context("entering alternate screen")?;
    crossterm::terminal::enable_raw_mode().context("enabling terminal raw mode")?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut ratatui_term = RatatuiTerminal::new(backend)?;
    ratatui_term.clear()?;

    // 7. Interactive Loop
    let mut recorded_events = Vec::new();
    let start_time = Instant::now();
    let mut last_event_time = start_time;
    let mut char_buffer = String::new();
    let mut mouse_capture_active = false;
    let mut scanner = MouseModeScanner::new();

    // Mouse tracking variables
    let mut current_mouse_col = 0u16;
    let mut current_mouse_row = 0u16;
    let mut is_dragging = false;
    let mut drag_start_col = 0u16;
    let mut drag_start_row = 0u16;

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

                        // Capture and redraw host terminal screen
                        let elapsed = start_time.elapsed();
                        if let Ok((frame, _)) = capture(
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
                            let _ = draw_terminal_state(&mut ratatui_term, &frame, elapsed);
                        }

                        // Scan for mouse mode updates
                        if let Some(enable) = scanner.scan(&data) {
                            if enable != mouse_capture_active {
                                mouse_capture_active = enable;
                                if enable {
                                    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
                                } else {
                                    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            recv(event_rx) -> res => {
                match res {
                    Ok(event) => {
                        match event {
                            crossterm::event::Event::Key(key_event) => {
                                if key_event.kind == crossterm::event::KeyEventKind::Release {
                                    continue;
                                }

                                let (named_key, mods) = map_crossterm_key(key_event);
                                let key_spec = KeySpec { key: named_key, mods };

                                let now = Instant::now();
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

                                if let Ok(bytes) = translator.encode(&key_spec, KeyAction::Press, &terminal) {
                                    pty.write(bytes);
                                }
                            }
                            crossterm::event::Event::Mouse(mouse_event) => {
                                if mouse_capture_active {
                                    if let Some((action, button)) = map_crossterm_mouse(mouse_event.kind) {
                                        let col = mouse_event.column;
                                        let row = mouse_event.row;

                                        // Translate row coordinate: subtract 2 header rows
                                        if row < 2 {
                                            continue; // ignore clicks on header
                                        }
                                        let row = row - 2;

                                        if col >= cols || row >= rows {
                                            continue; // ignore out-of-bounds clicks
                                        }

                                        let now = Instant::now();
                                        flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);

                                        // Move first if mouse pointer moved
                                        if (col != current_mouse_col || row != current_mouse_row) && !is_dragging {
                                            let move_idle = now.duration_since(last_event_time);
                                            if move_idle > Duration::from_millis(50) {
                                                recorded_events.push(Event::Sleep(move_idle));
                                            }
                                            recorded_events.push(Event::MouseMove {
                                                start_col: current_mouse_col,
                                                start_row: current_mouse_row,
                                                end_col: col,
                                                end_row: row,
                                                delay: Duration::from_millis(50),
                                                easing: None,
                                            });
                                            current_mouse_col = col;
                                            current_mouse_row = row;
                                            last_event_time = now;
                                        }

                                        let current_idle = now.duration_since(last_event_time);
                                        if current_idle > Duration::from_millis(50) {
                                            recorded_events.push(Event::Sleep(current_idle));
                                        }

                                        // Aggregate clicks and drags
                                        match action {
                                            MouseAction::Press => {
                                                if button == Some(MouseButton::Left) {
                                                    is_dragging = true;
                                                    drag_start_col = col;
                                                    drag_start_row = row;
                                                }
                                            }
                                            MouseAction::Release => {
                                                if button == Some(MouseButton::Left) && is_dragging {
                                                    is_dragging = false;
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
                                            MouseAction::Motion => {}
                                        }
                                        last_event_time = now;

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
                            }
                            _ => {}
                        }
                    }
                    Err(_) => break,
                }
            }
            recv(ticker) -> _ => {
                let elapsed = start_time.elapsed();
                if let Ok((frame, _)) = capture(
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
                    // Update host screen via Ratatui
                    let _ = draw_terminal_state(&mut ratatui_term, &frame, elapsed);

                    // Send to background renderer
                    let _ = renderer_tx.try_send(frame);
                }
            }
        }
    }

    // 8. Restore terminal raw mode and leave alternate screen
    if mouse_capture_active {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);

    // Flush any remaining buffered characters
    flush_char_buffer(&mut char_buffer, &mut recorded_events, &mut last_event_time);

    // 9. Format and write `.tape` file
    let mut tape_content = String::new();
    tape_content.push_str("# This is a reference tape to help you write your tape files.\n");
    tape_content.push_str("# We recommend viewing this tape using the Elixir language type for syntax highlighting.\n");
    tape_content.push_str("#\n");
    for line in REF_SCRIPT.lines() {
        tape_content.push_str(&format!("# {}\n", line));
    }
    tape_content.push_str("\n");

    // Output and configurations
    tape_content.push_str(&format!("Output {}\n", gif_path.display()));
    if let Some(sh) = &shell {
        tape_content.push_str(&format!("Set Shell {}\n", sh));
    }
    tape_content.push_str(&format!("Set Cols {}\n", cols));
    tape_content.push_str(&format!("Set Rows {}\n", rows));
    tape_content.push_str("Set FontSize 20\n");
    if let Some(theme) = &theme_name {
        tape_content.push_str(&format!("Set Theme \"{}\"\n", theme));
    }
    tape_content.push_str("\n");

    for event in &recorded_events {
        match event {
            Event::Type { text, delay } => {
                let escaped_text = text.replace('\\', "\\\\").replace('"', "\\\"");
                if *delay == Duration::from_millis(50) {
                    tape_content.push_str(&format!("Type \"{}\"\n", escaped_text));
                } else {
                    tape_content.push_str(&format!(
                        "Type@{}ms \"{}\"\n",
                        delay.as_millis(),
                        escaped_text
                    ));
                }
            }
            Event::Sleep(duration) => {
                if duration.as_millis() >= 1000 && duration.as_millis() % 100 == 0 {
                    tape_content.push_str(&format!("Sleep {}s\n", duration.as_secs_f32()));
                } else {
                    tape_content.push_str(&format!("Sleep {}ms\n", duration.as_millis()));
                }
            }
            Event::Key {
                key,
                action,
                count,
                ..
            } => {
                if *action == KeyAction::Press {
                    let mut spec_parts = Vec::new();
                    if key.mods.ctrl {
                        spec_parts.push("Ctrl");
                    }
                    if key.mods.alt {
                        spec_parts.push("Alt");
                    }
                    if key.mods.shift {
                        spec_parts.push("Shift");
                    }
                    if key.mods.super_key {
                        spec_parts.push("Super");
                    }

                    let key_name = match key.key {
                        NamedKey::Enter => "Enter".to_string(),
                        NamedKey::Escape => "Escape".to_string(),
                        NamedKey::Tab => "Tab".to_string(),
                        NamedKey::Backspace => "Backspace".to_string(),
                        NamedKey::Delete => "Delete".to_string(),
                        NamedKey::Insert => "Insert".to_string(),
                        NamedKey::Space => "Space".to_string(),
                        NamedKey::Up => "Up".to_string(),
                        NamedKey::Down => "Down".to_string(),
                        NamedKey::Left => "Left".to_string(),
                        NamedKey::Right => "Right".to_string(),
                        NamedKey::PageUp => "PageUp".to_string(),
                        NamedKey::PageDown => "PageDown".to_string(),
                        NamedKey::Home => "Home".to_string(),
                        NamedKey::End => "End".to_string(),
                        NamedKey::Char(c) => {
                            if c.is_alphabetic() {
                                c.to_ascii_uppercase().to_string()
                            } else {
                                c.to_string()
                            }
                        }
                        _ => " ".to_string(),
                    };

                    let mut cmd = spec_parts.join("+");
                    if !cmd.is_empty() {
                        cmd.push('+');
                    }
                    cmd.push_str(&key_name);

                    if *count > 1 {
                        tape_content.push_str(&format!("{} {}\n", cmd, count));
                    } else {
                        tape_content.push_str(&format!("{}\n", cmd));
                    }
                }
            }
            Event::Click { col, row, .. } => {
                tape_content.push_str(&format!("Click {} {}\n", col, row));
            }
            Event::RightClick { col, row, .. } => {
                tape_content.push_str(&format!("RightClick {} {}\n", col, row));
            }
            Event::MouseDrag {
                start_col,
                start_row,
                end_col,
                end_row,
                ..
            } => {
                tape_content.push_str(&format!(
                    "MouseDrag {} {} {} {}\n",
                    start_col, start_row, end_col, end_row
                ));
            }
            Event::MouseMove {
                start_col,
                start_row,
                end_col,
                end_row,
                ..
            } => {
                tape_content.push_str(&format!(
                    "MouseMove {} {} {} {}\n",
                    start_col, start_row, end_col, end_row
                ));
            }
            Event::MouseScroll {
                col,
                row,
                direction,
                ..
            } => {
                let dir_str = match direction {
                    crate::script::ScrollDirection::Up => "Up",
                    crate::script::ScrollDirection::Down => "Down",
                };
                tape_content.push_str(&format!(
                    "MouseScroll {} {} {}\n",
                    col, row, dir_str
                ));
            }
            _ => {}
        }
    }

    std::fs::write(&tape_path, tape_content)
        .with_context(|| format!("writing tape file to {}", tape_path.display()))?;

    // Drop the renderer tx to signal EOF, then await completion
    drop(renderer_tx);
    renderer_handle
        .join()
        .context("awaiting background GIF renderer compilation")?;

    info!(
        tape = %tape_path.display(),
        gif = %gif_path.display(),
        "recording completed successfully"
    );
    Ok(())
}
