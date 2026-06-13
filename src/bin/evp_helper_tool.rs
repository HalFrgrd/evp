use std::{
    io::{Write, stdout},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{
        self, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{self, ClearType},
};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = if args.len() < 2 {
        "key"
    } else {
        args[1].as_str()
    };

    match mode {
        "key" => run_key_mode(),
        "mouse" => run_mouse_mode(),
        _ => {
            eprintln!("Usage: {} [key|mouse]", args[0]);
            std::process::exit(1);
        }
    }
}

fn run_key_mode() -> std::io::Result<()> {
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

    writeln!(out, "ready\r")?;
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
        writeln!(out, "ready\r")?;
        writeln!(out, "counter={seen} key={key:?}\r")?;
        out.flush()?;
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}

fn run_mouse_mode() -> std::io::Result<()> {
    use crossterm::{
        event::{MouseEventKind, MouseButton},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen},
        style::{self, Color},
        QueueableCommand,
    };

    let mut out = stdout();
    terminal::enable_raw_mode()?;
    execute!(
        out,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        cursor::Hide
    )?;

    let (mut cols, mut rows) = terminal::size()?;
    let mut grid = vec![Color::Reset; (cols as usize) * (rows as usize)];

    // Helper to get character at (col, row)
    let get_char = |c: u16, r: u16, c_cols: u16, c_rows: u16| -> char {
        let legend = "L: Red | R: Green | Drag: Purple | Move: L.Blue | q: Exit";
        let legend_chars: Vec<char> = legend.chars().collect();
        let legend_len = legend_chars.len() as u16;
        if r == c_rows / 2 && c_cols >= legend_len {
            let start = (c_cols - legend_len) / 2;
            if c >= start && c < start + legend_len {
                return legend_chars[(c - start) as usize];
            }
        }
        ' '
    };

    // Helper to draw the entire grid
    let draw_all = |out: &mut std::io::Stdout, grid: &[Color], c_cols: u16, c_rows: u16| -> std::io::Result<()> {
        for r in 0..c_rows {
            out.queue(cursor::MoveTo(0, r))?;
            for c in 0..c_cols {
                let color = grid[(r * c_cols + c) as usize];
                out.queue(style::SetBackgroundColor(color))?;
                let ch = get_char(c, r, c_cols, c_rows);
                out.queue(style::Print(ch))?;
            }
        }
        out.queue(style::ResetColor)?;
        out.flush()?;
        Ok(())
    };

    draw_all(&mut out, &grid, cols, rows)?;

    loop {
        let event = event::read()?;
        match event {
            Event::Key(key) => {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
            Event::Resize(new_cols, new_rows) => {
                if new_cols != cols || new_rows != rows {
                    let mut new_grid = vec![Color::Reset; (new_cols as usize) * (new_rows as usize)];
                    for r in 0..(rows.min(new_rows)) {
                        for c in 0..(cols.min(new_cols)) {
                            new_grid[(r * new_cols + c) as usize] = grid[(r * cols + c) as usize];
                        }
                    }
                    grid = new_grid;
                    cols = new_cols;
                    rows = new_rows;
                    draw_all(&mut out, &grid, cols, rows)?;
                }
            }
            Event::Mouse(mouse_event) => {
                let col = mouse_event.column;
                let row = mouse_event.row;
                if col < cols && row < rows {
                    let new_color = match mouse_event.kind {
                        MouseEventKind::Down(MouseButton::Left) => Some(Color::Red),
                        MouseEventKind::Down(MouseButton::Right) => Some(Color::Green),
                        MouseEventKind::Drag(_) => Some(Color::Magenta),
                        MouseEventKind::Moved => Some(Color::Rgb { r: 173, g: 216, b: 230 }),
                        _ => None,
                    };
                    if let Some(color) = new_color {
                        let idx = (row * cols + col) as usize;
                        if grid[idx] != color {
                            grid[idx] = color;
                            out.queue(cursor::MoveTo(col, row))?;
                            out.queue(style::SetBackgroundColor(color))?;
                            let ch = get_char(col, row, cols, rows);
                            out.queue(style::Print(ch))?;
                            out.queue(style::ResetColor)?;
                            out.flush()?;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    execute!(
        out,
        event::DisableMouseCapture,
        LeaveAlternateScreen,
        cursor::Show
    )?;
    terminal::disable_raw_mode()?;
    Ok(())
}
