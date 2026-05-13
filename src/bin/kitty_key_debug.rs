use std::{
    io::{Write, stdout},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        ModifierKeyCode, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{self, ClearType},
};

fn main() -> std::io::Result<()> {
    let mut out = stdout();
    let max_keys = std::env::var("EVP_KITTY_KEY_EVENTS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(6);

    terminal::enable_raw_mode()?;
    execute!(
        out,
        terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    )?;

    writeln!(out, "ready")?;
    out.flush()?;

    let mut seen = 0usize;
    while seen < max_keys {
        if !event::poll(Duration::from_secs(5))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        seen += 1;
        execute!(out, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        writeln!(out, "ready")?;
        writeln!(out, "{}", format_key_debug_line(&key, seen))?;
        out.flush()?;
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}

fn format_key_debug_line(key: &KeyEvent, counter: usize) -> String {
    let code = key_code_name(key.code);
    let modifiers = key_modifiers_name(key.modifiers);
    let kind = key_kind_name(key.kind);
    if kind == "Press" {
        format!("key code({code}, modifiers={modifiers}) counter={counter}")
    } else {
        format!("key code({code}, modifiers={modifiers}, kind={kind}) counter={counter}")
    }
}

fn key_code_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Null => "null".to_string(),
        KeyCode::CapsLock => "capslock".to_string(),
        KeyCode::ScrollLock => "scrolllock".to_string(),
        KeyCode::NumLock => "numlock".to_string(),
        KeyCode::PrintScreen => "printscreen".to_string(),
        KeyCode::Pause => "pause".to_string(),
        KeyCode::Menu => "menu".to_string(),
        KeyCode::KeypadBegin => "keypadbegin".to_string(),
        KeyCode::Media(_) => "media".to_string(),
        KeyCode::Modifier(modifier) => match modifier {
            ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => "shift".to_string(),
            ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => "control".to_string(),
            ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => "alt".to_string(),
            ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => "super".to_string(),
            ModifierKeyCode::LeftHyper | ModifierKeyCode::RightHyper => "hyper".to_string(),
            ModifierKeyCode::LeftMeta | ModifierKeyCode::RightMeta => "meta".to_string(),
            ModifierKeyCode::IsoLevel3Shift | ModifierKeyCode::IsoLevel5Shift => {
                "isolevelshift".to_string()
            }
        },
    }
}

fn key_modifiers_name(mods: KeyModifiers) -> String {
    let mut parts = Vec::new();
    if mods.contains(KeyModifiers::SHIFT) {
        parts.push("Shift");
    }
    if mods.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl");
    }
    if mods.contains(KeyModifiers::ALT) {
        parts.push("Alt");
    }
    if mods.contains(KeyModifiers::SUPER) {
        parts.push("Super");
    }
    if mods.contains(KeyModifiers::HYPER) {
        parts.push("Hyper");
    }
    if mods.contains(KeyModifiers::META) {
        parts.push("Meta");
    }
    if parts.is_empty() {
        "None".to_string()
    } else {
        parts.join("+")
    }
}

fn key_kind_name(kind: KeyEventKind) -> &'static str {
    match kind {
        KeyEventKind::Press => "Press",
        KeyEventKind::Repeat => "Repeat",
        KeyEventKind::Release => "Release",
    }
}
