use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use libghostty_vt::{
    Terminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
};
use ratatui::{
    Terminal as RatatuiTerminal,
    backend::TerminaBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tracing::{info, warn};
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
use easing_function::easings::StandardEasing;

/// Dynamic mouse coordinate encoder using libghostty's mouse event protocol
fn encode_mouse_event(
    action: MouseAction,
    button: Option<MouseButton>,
    col: u16,
    row: u16,
    pixel_coords: Option<(u16, u16)>,
    terminal: &Terminal<'_, '_>,
    cell_width_px: u32,
    cell_height_px: u32,
    cols: u16,
    rows: u16,
) -> Result<Vec<u8>> {
    let pos = if let Some((x_px, y_px)) = pixel_coords {
        libghostty_vt::mouse::Position {
            x: x_px as f32,
            y: y_px as f32,
        }
    } else {
        let x = (col as f32 * cell_width_px as f32) + (cell_width_px as f32 / 2.0);
        let y = (row as f32 * cell_height_px as f32) + (cell_height_px as f32 / 2.0);
        libghostty_vt::mouse::Position { x, y }
    };

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

fn map_termina_key(event: termina::event::KeyEvent) -> Option<(NamedKey, ModSet)> {
    let key = match event.code {
        termina::event::KeyCode::Char(c) => NamedKey::Char(c),
        termina::event::KeyCode::Enter => NamedKey::Enter,
        termina::event::KeyCode::Tab => NamedKey::Tab,
        termina::event::KeyCode::Backspace => NamedKey::Backspace,
        termina::event::KeyCode::Delete => NamedKey::Delete,
        termina::event::KeyCode::Insert => NamedKey::Insert,
        termina::event::KeyCode::Escape => NamedKey::Escape,
        termina::event::KeyCode::Up => NamedKey::Up,
        termina::event::KeyCode::Down => NamedKey::Down,
        termina::event::KeyCode::Left => NamedKey::Left,
        termina::event::KeyCode::Right => NamedKey::Right,
        termina::event::KeyCode::PageUp => NamedKey::PageUp,
        termina::event::KeyCode::PageDown => NamedKey::PageDown,
        termina::event::KeyCode::Home => NamedKey::Home,
        termina::event::KeyCode::End => NamedKey::End,
        termina::event::KeyCode::Modifier(m) => match m {
            termina::event::ModifierKeyCode::LeftShift
            | termina::event::ModifierKeyCode::RightShift => NamedKey::Shift,
            termina::event::ModifierKeyCode::LeftControl
            | termina::event::ModifierKeyCode::RightControl => NamedKey::Control,
            termina::event::ModifierKeyCode::LeftAlt
            | termina::event::ModifierKeyCode::RightAlt => NamedKey::Alt,
            termina::event::ModifierKeyCode::LeftSuper
            | termina::event::ModifierKeyCode::RightSuper => NamedKey::Super,
            _ => return None,
        },
        _ => return None,
    };

    let mods = ModSet {
        ctrl: event.modifiers.contains(termina::event::Modifiers::CONTROL),
        alt: event.modifiers.contains(termina::event::Modifiers::ALT),
        shift: event.modifiers.contains(termina::event::Modifiers::SHIFT),
        super_key: event.modifiers.contains(termina::event::Modifiers::SUPER),
    };

    Some((key, mods))
}

fn map_termina_mods(modifiers: termina::event::Modifiers) -> ModSet {
    ModSet {
        ctrl: modifiers.contains(termina::event::Modifiers::CONTROL),
        alt: modifiers.contains(termina::event::Modifiers::ALT),
        shift: modifiers.contains(termina::event::Modifiers::SHIFT),
        super_key: modifiers.contains(termina::event::Modifiers::SUPER),
    }
}

fn map_termina_mouse(
    kind: termina::event::MouseEventKind,
) -> Option<(MouseAction, Option<MouseButton>)> {
    match kind {
        termina::event::MouseEventKind::Down(btn) => {
            Some((MouseAction::Press, map_mouse_button(btn)))
        }
        termina::event::MouseEventKind::Up(btn) => {
            Some((MouseAction::Release, map_mouse_button(btn)))
        }
        termina::event::MouseEventKind::Drag(btn) => {
            Some((MouseAction::Motion, map_mouse_button(btn)))
        }
        termina::event::MouseEventKind::Moved => Some((MouseAction::Motion, None)),
        termina::event::MouseEventKind::ScrollUp => {
            Some((MouseAction::Press, Some(MouseButton::WheelUp)))
        }
        termina::event::MouseEventKind::ScrollDown => {
            Some((MouseAction::Press, Some(MouseButton::WheelDown)))
        }
        termina::event::MouseEventKind::ScrollLeft
        | termina::event::MouseEventKind::ScrollRight => None,
    }
}

fn map_mouse_button(btn: termina::event::MouseButton) -> Option<MouseButton> {
    match btn {
        termina::event::MouseButton::Left => Some(MouseButton::Left),
        termina::event::MouseButton::Right => Some(MouseButton::Right),
        termina::event::MouseButton::Middle => Some(MouseButton::Middle),
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

fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0B".to_string();
    }
    let units = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut bytes_f = bytes as f64;
    let mut i = 0;
    while bytes_f >= 1000.0 && i < units.len() - 1 {
        bytes_f /= 1000.0;
        i += 1;
    }

    let formatted = if bytes_f >= 99.95 {
        format!("{:.0}", bytes_f)
    } else if bytes_f >= 9.995 {
        format!("{:.1}", bytes_f)
    } else {
        format!("{:.2}", bytes_f)
    };

    if formatted.starts_with("1000") && i < units.len() - 1 {
        format!("1.00{}", units[i + 1])
    } else {
        format!("{}{}", formatted, units[i])
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
    mouse_event_info: Option<&str>,
    out_filename: &str,
    out_size: u64,
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
    let size_str = format_size(out_size);
    let prefix_text = format!(
        " EVP recording active ({}) [{}: {}]. To stop recording, exit the program or ",
        seconds_str, out_filename, size_str
    );
    let click_text = "click here";
    let suffix_text = if mouse_event_info.is_some() {
        ". "
    } else {
        "."
    };

    let mut parts = vec![
        (dot_text.to_string(), dot_style, false),
        (prefix_text, normal_style, false),
        (click_text.to_string(), click_style, true),
        (suffix_text.to_string(), normal_style, false),
    ];
    if let Some(info) = mouse_event_info {
        parts.push((info.to_string(), normal_style, false));
    }

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
    ratatui_term: &mut RatatuiTerminal<TerminaBackend<termina::PlatformTerminal>>,
    frame: &crate::recording::RawFrame,
    elapsed: Duration,
    host_mouse_pos: Option<(u16, u16)>,
    mouse_event_info: Option<&str>,
    out_filename: &str,
    out_size: u64,
) -> Result<(u16, Vec<(u16, u16)>)> {
    let mut click_cells = Vec::new();
    let mut header_height = 1u16;

    ratatui_term.draw(|f| {
        let show_dot = (elapsed.as_millis() % 1000) < 500;

        // 1. Dry run to calculate header height and hover state
        let is_hovered = if let Some((m_col, m_row)) = host_mouse_pos {
            let dry_run = layout_header(
                None,
                f.area(),
                elapsed,
                show_dot,
                false,
                mouse_event_info,
                out_filename,
                out_size,
            );
            dry_run.click_cells.contains(&(m_col, m_row))
        } else {
            false
        };

        let dry_run = layout_header(
            None,
            f.area(),
            elapsed,
            show_dot,
            is_hovered,
            mouse_event_info,
            out_filename,
            out_size,
        );
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
            mouse_event_info,
            out_filename,
            out_size,
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

fn perpendicular_distance(p: (u16, u16), line_start: (u16, u16), line_end: (u16, u16)) -> f32 {
    let dx = line_end.0 as f32 - line_start.0 as f32;
    let dy = line_end.1 as f32 - line_start.1 as f32;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 0.01 {
        let diff_x = p.0 as f32 - line_start.0 as f32;
        let diff_y = p.1 as f32 - line_start.1 as f32;
        return (diff_x * diff_x + diff_y * diff_y).sqrt();
    }
    let num = (dy * p.0 as f32 - dx * p.1 as f32 + line_end.0 as f32 * line_start.1 as f32
        - line_end.1 as f32 * line_start.0 as f32)
        .abs();
    num / len_sq.sqrt()
}

fn rdp_simplify(points: &[(u16, u16, Instant)], epsilon: f32) -> Vec<(u16, u16, Instant)> {
    if points.len() < 3 {
        return points.to_vec();
    }

    let mut dmax = 0.0;
    let mut index = 0;
    let end = points.len() - 1;
    let line_start = (points[0].0, points[0].1);
    let line_end = (points[end].0, points[end].1);

    for i in 1..end {
        let p = (points[i].0, points[i].1);
        let dist = perpendicular_distance(p, line_start, line_end);
        if dist > dmax {
            index = i;
            dmax = dist;
        }
    }

    if dmax > epsilon {
        let mut results1 = rdp_simplify(&points[0..=index], epsilon);
        let results2 = rdp_simplify(&points[index..], epsilon);
        results1.pop();
        results1.extend(results2);
        results1
    } else {
        vec![points[0], points[end]]
    }
}

fn find_best_easing(points: &[(u16, u16, Instant)]) -> StandardEasing {
    if points.len() < 3 {
        return StandardEasing::Linear;
    }

    let t0 = points.first().unwrap().2;
    let tn = points.last().unwrap().2;
    let total_duration = tn.duration_since(t0);
    if total_duration.is_zero() {
        return StandardEasing::Linear;
    }

    let p0 = (
        points.first().unwrap().0 as f32,
        points.first().unwrap().1 as f32,
    );
    let pn = (
        points.last().unwrap().0 as f32,
        points.last().unwrap().1 as f32,
    );
    let total_dist = ((pn.0 - p0.0).powi(2) + (pn.1 - p0.1).powi(2)).sqrt();
    if total_dist < 0.001 {
        return StandardEasing::Linear;
    }

    let candidates = [
        StandardEasing::Linear,
        StandardEasing::InSine,
        StandardEasing::OutSine,
        StandardEasing::InOutSine,
        StandardEasing::InQuadradic,
        StandardEasing::OutQuadradic,
        StandardEasing::InOutQuadradic,
        StandardEasing::InCubic,
        StandardEasing::OutCubic,
        StandardEasing::InOutCubic,
        StandardEasing::InQuartic,
        StandardEasing::OutQuartic,
        StandardEasing::InOutQuartic,
        StandardEasing::InQuintic,
        StandardEasing::OutQuintic,
        StandardEasing::InOutQuintic,
        StandardEasing::InExponential,
        StandardEasing::OutExponential,
        StandardEasing::InOutExponential,
        StandardEasing::InCircular,
        StandardEasing::OutCircular,
        StandardEasing::InOutCircular,
        StandardEasing::InBack,
        StandardEasing::OutBack,
        StandardEasing::InOutBack,
        StandardEasing::InElastic,
        StandardEasing::OutElastic,
        StandardEasing::InOutElastic,
        StandardEasing::InBounce,
        StandardEasing::OutBounce,
        StandardEasing::InOutBounce,
    ];

    use easing_function::Easing;

    let mut best_easing = StandardEasing::Linear;
    let mut min_sse = f32::MAX;

    for &easing in &candidates {
        let mut sse = 0.0;
        for &(x, y, time) in points {
            let t = time.duration_since(t0).as_secs_f32() / total_duration.as_secs_f32();
            let t = t.clamp(0.0, 1.0);

            let dx = pn.0 - p0.0;
            let dy = pn.1 - p0.1;
            let px = x as f32 - p0.0;
            let py = y as f32 - p0.1;
            let u = if total_dist > 0.0 {
                (px * dx + py * dy) / (total_dist * total_dist)
            } else {
                0.0
            };
            let u = u.clamp(0.0, 1.0);

            let eased_t = easing.ease(t);
            sse += (u - eased_t).powi(2);
        }
        if sse < min_sse {
            min_sse = sse;
            best_easing = easing;
        }
    }

    best_easing
}

/// Accumulates a continuous sequence of mouse movements and flushes a single simplified
/// MouseMove or MouseDrag event when collinearity is broken or a pause of >1s occurs.
struct MouseSegmentTracker {
    points: Vec<(u16, u16, Instant)>,
    is_drag: bool,
    mods: ModSet,
    track_pixels: bool,
    cell_width: u32,
    cell_height: u32,
}

impl MouseSegmentTracker {
    fn new(
        is_drag: bool,
        mods: ModSet,
        track_pixels: bool,
        cell_width: u32,
        cell_height: u32,
    ) -> Self {
        Self {
            points: Vec::new(),
            is_drag,
            mods,
            track_pixels,
            cell_width,
            cell_height,
        }
    }

    fn add_point(&mut self, col: u16, row: u16, now: Instant) -> Option<Event> {
        if self.points.is_empty() {
            self.points.push((col, row, now));
            return None;
        }

        let last_point = *self.points.last().unwrap();

        // 1. If paused for more than 500ms, break to a new segment
        if now.duration_since(last_point.2) > Duration::from_millis(500) {
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

        self.points.push((col, row, now));
        None
    }

    fn flush(&mut self) -> Option<Event> {
        if self.points.len() < 2 {
            self.points.clear();
            return None;
        }

        let simplified = rdp_simplify(&self.points, 1.5);
        if simplified.len() < 2 {
            self.points.clear();
            return None;
        }

        let start = simplified.first().unwrap();
        let end = simplified.last().unwrap();
        let duration = end.2.duration_since(start.2);
        let delay = if duration < Duration::from_millis(50) {
            Duration::from_millis(50)
        } else {
            duration
        };

        let mut coords = Vec::with_capacity(simplified.len());
        for p in &simplified {
            let col = if self.track_pixels {
                (p.0 as u32 / self.cell_width) as u16
            } else {
                p.0
            };
            let row = if self.track_pixels {
                (p.1 as u32 / self.cell_height) as u16
            } else {
                p.1
            };
            coords.push((col, row));
        }

        let best_easing = find_best_easing(&self.points);

        let ev = if self.is_drag {
            Some(Event::MouseDrag {
                coords,
                mods: self.mods,
                delay,
                easing: Some(best_easing),
            })
        } else {
            Some(Event::MouseMove {
                coords,
                mods: self.mods,
                delay,
                easing: Some(best_easing),
            })
        };

        self.points.clear();
        ev
    }
}

struct TerminalCapabilityGuard;

impl Drop for TerminalCapabilityGuard {
    fn drop(&mut self) {
        use std::io::Write;
        use termina::Terminal;
        use termina::escape::csi::{Csi, DecPrivateMode, DecPrivateModeCode, Keyboard, Mode};

        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Keyboard(Keyboard::PopFlags(1))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::BracketedPaste
            )))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::MouseTracking
            )))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ButtonEventMouse
            )))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::AnyEventMouse
            )))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SGRMouse
            )))
        );
        let _ = write!(
            std::io::stdout(),
            "{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ClearAndEnableAlternateScreen
            )))
        );
        let _ = std::io::stdout().flush();

        if let Ok(mut term) = termina::PlatformTerminal::new() {
            let _ = term.enter_cooked_mode();
        }
        crate::telemetry::SUSPEND_LOGGING.store(false, std::sync::atomic::Ordering::SeqCst);
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
    fps: u64,
) -> Result<()> {
    // 1. Resolve geometry
    use termina::Terminal as TerminaTerminalTrait;
    let mut host_terminal =
        termina::PlatformTerminal::new().context("creating PlatformTerminal")?;
    let ws = host_terminal
        .get_dimensions()
        .context("getting host terminal size")?;
    let actual_host_cols = ws.cols;
    let actual_host_rows = ws.rows;

    let mut cols = override_cols.unwrap_or(actual_host_cols);
    let mut rows = override_rows.unwrap_or(actual_host_rows.saturating_sub(2));

    if rows == 0 {
        bail!("terminal height is too small for EVP recording");
    }

    // Configure raw mode and terminal capabilities
    host_terminal
        .enter_raw_mode()
        .context("entering raw mode")?;
    crate::telemetry::SUSPEND_LOGGING.store(true, std::sync::atomic::Ordering::SeqCst);
    use std::io::Write;
    use termina::escape::csi::{
        Csi, DecModeSetting, DecPrivateMode, DecPrivateModeCode, Keyboard, KittyKeyboardFlags, Mode,
    };

    // Query if SGRPixelsMouse is supported (DEC private mode 1016)
    let mut supports_sgr_pixels = false;
    // if write!(
    //     host_terminal,
    //     "{}",
    //     Csi::Mode(Mode::QueryDecPrivateMode(DecPrivateMode::Code(
    //         DecPrivateModeCode::SGRPixelsMouse
    //     )))
    // )
    // .is_ok()
    //     && host_terminal.flush().is_ok()
    // {
    //     // Wait up to 300ms for a response
    //     if let Ok(true) = host_terminal.poll(
    //         |event| {
    //             matches!(
    //                 event,
    //                 termina::Event::Csi(Csi::Mode(Mode::ReportDecPrivateMode {
    //                     mode: DecPrivateMode::Code(DecPrivateModeCode::SGRPixelsMouse),
    //                     ..
    //                 }))
    //             )
    //         },
    //         Some(Duration::from_millis(300)),
    //     ) {
    //         if let Ok(termina::Event::Csi(Csi::Mode(Mode::ReportDecPrivateMode {
    //             setting, ..
    //         }))) = host_terminal.read(|_| true)
    //         {
    //             if setting != DecModeSetting::NotRecognized {
    //                 supports_sgr_pixels = true;
    //             }
    //         }
    //     }
    // }

    write!(
        host_terminal,
        "{}",
        Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
            DecPrivateModeCode::ClearAndEnableAlternateScreen
        )))
    )?;
    write!(
        host_terminal,
        "{}",
        Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
            DecPrivateModeCode::MouseTracking
        )))
    )?;
    write!(
        host_terminal,
        "{}",
        Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
            DecPrivateModeCode::ButtonEventMouse
        )))
    )?;
    write!(
        host_terminal,
        "{}",
        Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
            DecPrivateModeCode::AnyEventMouse
        )))
    )?;

    if supports_sgr_pixels {
        write!(
            host_terminal,
            "{}",
            Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SGRPixelsMouse
            )))
        )?;
    } else {
        write!(
            host_terminal,
            "{}",
            Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SGRMouse
            )))
        )?;
    }
    write!(
        host_terminal,
        "{}",
        Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
            DecPrivateModeCode::BracketedPaste
        )))
    )?;

    let flags = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
        | KittyKeyboardFlags::REPORT_EVENT_TYPES
        | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS
        | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    write!(
        host_terminal,
        "{}",
        Csi::Keyboard(Keyboard::PushFlags(flags))
    )?;
    host_terminal.flush()?;

    let _guard = TerminalCapabilityGuard;

    let mut settings = Settings::default();
    settings.cols = Some(cols);
    settings.rows = Some(rows);
    settings.framerate = fps as u32;
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

    terminal.on_title_changed(|term| {
        if let Ok(title) = term.title() {
            info!("Program changed window title to: {:?}", title);
        }
    })?;

    let mut osc_22_parser = crate::runner::Osc22Parser::new();
    let mut state_tracker = crate::runner::TerminalStateTracker::new();
    state_tracker.update_and_log(&terminal);

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
        theme: settings.theme.clone(),
    };
    let backend = renderer::RendererBackend::for_path(&out_path, &render_opts, true, false)?;

    let renderer_handle = renderer::spawn_renderer(cfg, backend, out_path.clone())
        .context("spawning background renderer")?;
    let renderer_tx = renderer_handle.tx.clone();

    // 5. Setup channels and multi-threaded event polling
    let reader = host_terminal.event_reader();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        loop {
            match reader.read(|_| true) {
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
        let backend = TerminaBackend::new(host_terminal);
        let mut ratatui_term = RatatuiTerminal::new(backend)?;
        // Render an empty frame to synchronize ratatui buffer state without deadlocking on get_cursor_position
        ratatui_term.draw(|_| {})?;

        // Mouse tracking variables
        let out_filename = out_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("output.gif");
        let mut current_mouse_col = 0u16;
        let mut current_mouse_row = 0u16;
        let mut is_dragging = false;
        let mut drag_start_col = 0u16;
        let mut drag_start_row = 0u16;
        let mut current_mouse_pos: Option<(f32, f32, MouseState)> = None;
        let mut last_mouse_move_time = start_time;
        let mut host_mouse_pos: Option<(u16, u16)> = None;
        let mut mouse_event_info: Option<String> = None;
        let mut click_here_cells: Vec<(u16, u16)> = Vec::new();
        let mut header_height = 1u16;

        let mut render_state = RenderState::new()?;
        let mut row_it = RowIterator::new()?;
        let mut cell_it = CellIterator::new()?;
        let mut last_cursor_moved_at = None;
        let mut prev_cursor_pos = None;

        loop {
            crossbeam_channel::select! {
                recv(pty_rx) -> res => {
                    match res {
                        Ok(data) => {
                            if data.is_empty() {
                                break; // EOF
                            }
                            for &b in &data {
                                osc_22_parser.feed(b, |shape| {
                                    info!("Program changed mouse pointer shape to: {:?}", shape);
                                });
                            }
                            // Feed output to libghostty VT parser
                            terminal.vt_write(&data);
                            state_tracker.update_and_log(&terminal);
                        }
                        Err(_) => break,
                    }
                }
                recv(event_rx) -> res => {
                    match res {
                        Ok(event) => {
                            match event {
                                termina::Event::Key(key_event) => {
                                    // Interrupt and flush active mouse movement
                                    if let Some(mut tracker) = active_tracker.take() {
                                        if let Some(ev) = tracker.flush() {
                                            recorded_events.push(ev);
                                        }
                                    }

                                    let Some((named_key, mods)) = map_termina_key(key_event) else {
                                        continue;
                                    };
                                    let key_spec = KeySpec { key: named_key, mods };
                                    let action = if key_event.kind == termina::event::KeyEventKind::Release {
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
                                termina::Event::Mouse(mouse_event) => {
                                    let col = if supports_sgr_pixels {
                                        (mouse_event.column as u32 / cfg.cell_width_px) as u16
                                    } else {
                                        mouse_event.column
                                    };
                                    let row = if supports_sgr_pixels {
                                        (mouse_event.row as u32 / cfg.cell_height_px) as u16
                                    } else {
                                        mouse_event.row
                                    };

                                    host_mouse_pos = Some((col, row));

                                    let info = if supports_sgr_pixels {
                                        format!("MouseEvent: {}x{} (pixels)", mouse_event.column, mouse_event.row)
                                    } else {
                                        format!("MouseEvent: {}x{} (grid)", mouse_event.column, mouse_event.row)
                                    };
                                    mouse_event_info = Some(info);

                                    let is_click_here = click_here_cells.contains(&(col, row));

                                    if is_click_here {
                                        if let termina::event::MouseEventKind::Down(termina::event::MouseButton::Left) = mouse_event.kind {
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
                                            &mut last_cursor_moved_at,
                                            &mut prev_cursor_pos,
                                            None,
                                        ) {
                                            frame.mouse_cursor = current_mouse_pos;
                                            let out_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                                            if let Ok((height, cells)) = draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos, mouse_event_info.as_deref(), out_filename, out_size) {
                                                header_height = height;
                                                click_here_cells = cells;
                                            }
                                        }
                                        continue;
                                    }

                                    // If we moved off "click here" to another part of the header/divider, redraw to clear hover
                                    if row < header_height + 1 {
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
                                            &mut last_cursor_moved_at,
                                            &mut prev_cursor_pos,
                                            None,
                                        ) {
                                            frame.mouse_cursor = current_mouse_pos;
                                            let out_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                                            if let Ok((height, cells)) = draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos, mouse_event_info.as_deref(), out_filename, out_size) {
                                                header_height = height;
                                                click_here_cells = cells;
                                            }
                                        }
                                        continue;
                                    }

                                    if let Some((action, button)) = map_termina_mouse(mouse_event.kind) {
                                            // Translate row coordinate: subtract header rows and divider row
                                            let pty_row = row.saturating_sub(header_height + 1);

                                            if col >= cols || pty_row >= rows {
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

                                            let cell_x = if supports_sgr_pixels {
                                                mouse_event.column as f32 / cfg.cell_width_px as f32
                                            } else {
                                                col as f32
                                            };
                                            let cell_y = if supports_sgr_pixels {
                                                (mouse_event.row as f32 / cfg.cell_height_px as f32) - (header_height + 1) as f32
                                            } else {
                                                pty_row as f32
                                            };
                                            current_mouse_pos = Some((cell_x, cell_y, mouse_state));

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
                                                    let tracker_mods = map_termina_mods(mouse_event.modifiers);
                                                    let mut tracker = MouseSegmentTracker::new(is_drag, tracker_mods, supports_sgr_pixels, cfg.cell_width_px, cfg.cell_height_px);
                                                    // Initialize segment starting point at the previous mouse coords
                                                    tracker.points.push((current_mouse_col, current_mouse_row, last_event_time));
                                                    active_tracker = Some(tracker);
                                                }

                                                let tracker_x = if supports_sgr_pixels { mouse_event.column } else { col };
                                                let tracker_y = if supports_sgr_pixels { mouse_event.row } else { pty_row };

                                                if let Some(ref mut tracker) = active_tracker {
                                                    if let Some(ev) = tracker.add_point(tracker_x, tracker_y, now) {
                                                        recorded_events.push(ev);
                                                    }
                                                }
                                                current_mouse_col = tracker_x;
                                                current_mouse_row = tracker_y;
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

                                                let ev_mods = map_termina_mods(mouse_event.modifiers);

                                                // Process instantaneous mouse event
                                                match action {
                                                    MouseAction::Press => {
                                                        if button == Some(MouseButton::Left) {
                                                            drag_start_col = col;
                                                            drag_start_row = pty_row;
                                                        }
                                                    }
                                                    MouseAction::Release => {
                                                        if button == Some(MouseButton::Left) {
                                                            if col == drag_start_col && pty_row == drag_start_row {
                                                                recorded_events.push(Event::Click {
                                                                    col,
                                                                    row: pty_row,
                                                                    mods: ev_mods,
                                                                    delay: Duration::from_millis(50),
                                                                });
                                                            } else {
                                                                recorded_events.push(Event::MouseDrag {
                                                                    coords: vec![(drag_start_col, drag_start_row), (col, pty_row)],
                                                                    mods: ev_mods,
                                                                    delay: Duration::from_millis(50),
                                                                    easing: Some(StandardEasing::InOutElastic),
                                                                });
                                                            }
                                                        } else if button == Some(MouseButton::Right) {
                                                            recorded_events.push(Event::RightClick {
                                                                col,
                                                                row: pty_row,
                                                                mods: ev_mods,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        } else if button == Some(MouseButton::WheelUp) {
                                                            recorded_events.push(Event::MouseScroll {
                                                                col,
                                                                row: pty_row,
                                                                direction: crate::script::ScrollDirection::Up,
                                                                mods: ev_mods,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        } else if button == Some(MouseButton::WheelDown) {
                                                            recorded_events.push(Event::MouseScroll {
                                                                col,
                                                                row: pty_row,
                                                                direction: crate::script::ScrollDirection::Down,
                                                                mods: ev_mods,
                                                                delay: Duration::from_millis(50),
                                                            });
                                                        }
                                                        current_mouse_col = if supports_sgr_pixels { mouse_event.column } else { col };
                                                        current_mouse_row = if supports_sgr_pixels { mouse_event.row } else { pty_row };
                                                    }
                                                    _ => {}
                                                }
                                                last_event_time = now;
                                            }

                                            let pixel_coords = if supports_sgr_pixels {
                                                let y_offset_px = (header_height + 1) as u32 * cfg.cell_height_px;
                                                Some((
                                                    mouse_event.column,
                                                    mouse_event.row.saturating_sub(y_offset_px as u16),
                                                ))
                                            } else {
                                                None
                                            };

                                            // Encode and transmit mouse coordinates to the PTY
                                            if let Ok(bytes) = encode_mouse_event(
                                                action,
                                                button,
                                                col,
                                                pty_row,
                                                pixel_coords,
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
                                termina::Event::Paste(text) => {
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
                                termina::Event::WindowResized(ws) => {
                                    let new_host_cols = ws.cols;
                                    let new_host_rows = ws.rows;
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
                    let _loop_timer = crate::telemetry::ScopeTimer::new("record_tick_total");
                    let elapsed = start_time.elapsed();
                    let capture_res = {
                        let _cap_timer = crate::telemetry::ScopeTimer::new("record_capture");
                        capture(
                            &mut render_state,
                            &mut row_it,
                            &mut cell_it,
                            &mut terminal,
                            elapsed,
                            cols,
                            rows,
                            true,
                            &mut last_cursor_moved_at,
                            &mut prev_cursor_pos,
                            None,
                        )
                    };
                    if let Ok((mut frame, _)) = capture_res {
                        if Instant::now().duration_since(last_mouse_move_time) > Duration::from_secs(3) {
                            current_mouse_pos = None;
                        }
                        // Inject mouse pointer into the frame so the GIF compiler renders it
                        frame.mouse_cursor = current_mouse_pos;

                        // Update host screen via Ratatui
                        let draw_res = {
                            let _draw_timer = crate::telemetry::ScopeTimer::new("record_draw_terminal_state");
                            let out_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                            draw_terminal_state(&mut ratatui_term, &frame, elapsed, host_mouse_pos, mouse_event_info.as_deref(), out_filename, out_size)
                        };
                        if let Ok((height, cells)) = draw_res {
                            header_height = height;
                            click_here_cells = cells;
                        }

                        // Send directly to background renderer
                        let _send_timer = crate::telemetry::ScopeTimer::new("record_send_frame");
                        let _ = renderer_tx.try_send(frame);
                    }
                }
            }
        }

        // Wait 50ms and discard any remaining terminal input events
        std::thread::sleep(std::time::Duration::from_millis(50));
        while event_rx.try_recv().is_ok() {}
    }

    drop(_guard);

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

    if let Err(e) = crate::script::parse_path(&tape_path) {
        warn!(
            "Generated tape file `{}` is invalid: {e:#}",
            tape_path.display()
        );
    }

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

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(999), "999B");
        assert_eq!(format_size(1000), "1.00kB");
        assert_eq!(format_size(1050), "1.05kB");
        assert_eq!(format_size(9880), "9.88kB");
        assert_eq!(format_size(34600), "34.6kB");
        assert_eq!(format_size(105000), "105kB");
        assert_eq!(format_size(999000), "999kB");
        assert_eq!(format_size(999900), "1.00MB");
        assert_eq!(format_size(1000000), "1.00MB");
        assert_eq!(format_size(34600000), "34.6MB");
    }
}
