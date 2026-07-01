//! VHS `.tape` script parsing.

use easing_function::easings::StandardEasing;
use std::path::Path;
use std::time::Duration;

pub mod ast;
pub mod parser;

pub use ast::{
    Event, KeyAction, KeySpec, ModSet, MouseAction, MouseButton, NamedKey, Script, ScrollDirection,
    Settings, WaitScope,
};
pub use parser::{parse, parse_path};

pub const REF_SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/ref_script.tape"));

/// Generates the standard commented-out reference tape header block.
pub fn write_reference_header() -> String {
    let mut header = String::new();
    header.push_str("# This is a reference tape to help you write your tape files.\n");
    header.push_str("# We recommend viewing this tape using the Elixir language type for syntax highlighting.\n");
    header.push_str("#\n");
    for line in REF_SCRIPT.lines() {
        header.push_str(&format!("# {}\n", line));
    }
    header
}

fn format_mouse_cmd(name: &str, delay: Duration, easing: Option<StandardEasing>) -> String {
    let mut cmd = name.to_string();
    let has_delay = delay != Duration::from_millis(50);
    if has_delay {
        cmd.push_str(&format!("@{}ms", delay.as_millis()));
    }
    if let Some(e) = easing {
        let easing_name = match e {
            StandardEasing::Linear => "Linear",
            StandardEasing::InSine => "EaseInSine",
            StandardEasing::OutSine => "EaseOutSine",
            StandardEasing::InOutSine => "EaseInOutSine",
            StandardEasing::InQuadradic => "EaseInQuad",
            StandardEasing::OutQuadradic => "EaseOutQuad",
            StandardEasing::InOutQuadradic => "EaseInOutQuad",
            StandardEasing::InCubic => "EaseInCubic",
            StandardEasing::OutCubic => "EaseOutCubic",
            StandardEasing::InOutCubic => "EaseInOutCubic",
            StandardEasing::InQuartic => "EaseInQuart",
            StandardEasing::OutQuartic => "EaseOutQuart",
            StandardEasing::InOutQuartic => "EaseInOutQuart",
            StandardEasing::InQuintic => "EaseInQuint",
            StandardEasing::OutQuintic => "EaseOutQuint",
            StandardEasing::InOutQuintic => "EaseInOutQuint",
            StandardEasing::InExponential => "EaseInExpo",
            StandardEasing::OutExponential => "EaseOutExpo",
            StandardEasing::InOutExponential => "EaseInOutExpo",
            StandardEasing::InCircular => "EaseInCirc",
            StandardEasing::OutCircular => "EaseOutCirc",
            StandardEasing::InOutCircular => "EaseInOutCirc",
            StandardEasing::InBack => "EaseInBack",
            StandardEasing::OutBack => "EaseOutBack",
            StandardEasing::InOutBack => "EaseInOutBack",
            StandardEasing::InElastic => "EaseInElastic",
            StandardEasing::OutElastic => "EaseOutElastic",
            StandardEasing::InOutElastic => "EaseInOutElastic",
            StandardEasing::InBounce => "EaseInBounce",
            StandardEasing::OutBounce => "EaseOutBounce",
            StandardEasing::InOutBounce => "EaseInOutBounce",
        };
        cmd.push_str(&format!("@{}", easing_name));
    }
    cmd
}

fn format_mods_prefix(mods: ModSet) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    if mods.super_key {
        parts.push("Super");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}+", parts.join("+"))
    }
}

/// Formats a list of recorded events and configurations into a complete, valid `.tape` script string.
pub fn write_tape_script(
    events: &[Event],
    out_path: &Path,
    shell: Option<&str>,
    cols: u16,
    rows: u16,
    theme: Option<&str>,
) -> String {
    let mut tape_content = write_reference_header();
    tape_content.push_str("\n");

    // Output and configurations
    tape_content.push_str(&format!("Output {}\n", out_path.display()));
    if let Some(sh) = &shell {
        tape_content.push_str(&format!("Set Shell {}\n", sh));
    }
    tape_content.push_str(&format!("Set Cols {}\n", cols));
    tape_content.push_str(&format!("Set Rows {}\n", rows));
    tape_content.push_str("Set FontSize 20\n");
    if let Some(theme_name) = &theme {
        tape_content.push_str(&format!("Set Theme \"{}\"\n", theme_name));
    }
    tape_content.push_str("\n");

    for event in events {
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
                key, action, count, ..
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
            Event::Click { col, row, mods, .. } => {
                tape_content.push_str(&format!(
                    "{}Click {} {}\n",
                    format_mods_prefix(*mods),
                    col,
                    row
                ));
            }
            Event::RightClick { col, row, mods, .. } => {
                tape_content.push_str(&format!(
                    "{}RightClick {} {}\n",
                    format_mods_prefix(*mods),
                    col,
                    row
                ));
            }
            Event::DoubleClick { col, row, mods, .. } => {
                tape_content.push_str(&format!(
                    "{}DoubleClick {} {}\n",
                    format_mods_prefix(*mods),
                    col,
                    row
                ));
            }
            Event::MouseDrag {
                start_col,
                start_row,
                end_col,
                end_row,
                mods,
                delay,
                easing,
            } => {
                tape_content.push_str(&format!(
                    "{}{} {} {} {} {}\n",
                    format_mods_prefix(*mods),
                    format_mouse_cmd("MouseDrag", *delay, *easing),
                    start_col,
                    start_row,
                    end_col,
                    end_row
                ));
            }
            Event::MouseMove {
                start_col,
                start_row,
                end_col,
                end_row,
                mods,
                delay,
                easing,
            } => {
                tape_content.push_str(&format!(
                    "{}{} {} {} {} {}\n",
                    format_mods_prefix(*mods),
                    format_mouse_cmd("MouseMove", *delay, *easing),
                    start_col,
                    start_row,
                    end_col,
                    end_row
                ));
            }
            Event::MouseScroll {
                col,
                row,
                direction,
                mods,
                ..
            } => {
                let dir_str = match direction {
                    crate::script::ScrollDirection::Up => "Up",
                    crate::script::ScrollDirection::Down => "Down",
                };
                tape_content.push_str(&format!(
                    "{}MouseScroll {} {} {}\n",
                    format_mods_prefix(*mods),
                    col,
                    row,
                    dir_str
                ));
            }
            _ => {}
        }
    }
    tape_content
}
