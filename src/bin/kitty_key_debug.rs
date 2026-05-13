use std::io::{Write, stdout};

use crossterm::{
    cursor,
    event::{
        Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags, read,
    },
    execute,
    terminal::{self, ClearType},
};

fn main() -> std::io::Result<()> {
    let mut out = stdout();
    let max_events = std::env::var("EVP_KITTY_KEY_EVENTS")
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
    while seen < max_events {
        if let Event::Key(key) = read()? {
            writeln!(
                out,
                "key code={:?} mods={:?} kind={:?}",
                key.code, key.modifiers, key.kind
            )?;
            out.flush()?;
            seen += 1;
        }
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}
