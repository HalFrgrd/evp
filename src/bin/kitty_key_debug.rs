use std::{
    io::{Write, stdout},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{
        self, Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
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
        writeln!(out, "counter={seen} key={key:?}")?;
        out.flush()?;
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}
