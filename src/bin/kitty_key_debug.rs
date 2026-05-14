use std::{
    io::{Write, stdout},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{
        self, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{self, ClearType},
};

fn main() -> std::io::Result<()> {
    let mut out = stdout();
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
    loop {
        if !event::poll(Duration::from_secs(5))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::NONE {
            break;
        }

        seen += 1;
        execute!(out, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        writeln!(out, "ready")?;
        writeln!(out, "counter={seen} key={key:?}")?;
        out.flush()?;
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}
