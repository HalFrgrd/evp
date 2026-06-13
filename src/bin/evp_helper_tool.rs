use std::{
    io::{Read, Write, stdout},
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
        "stress-test-program" | "stress_test_program" => run_stress_test_program(),
        _ => {
            eprintln!("Usage: {} [key|mouse|stress-test-program]", args[0]);
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

    const BASE_TEXT: &str = "Press any key sequence (q to quit)...";

    writeln!(out, "{}\r", BASE_TEXT)?;
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
        writeln!(out, "{}\r", BASE_TEXT)?;
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

struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        let state = if seed == 0 { 0xEEA } else { seed };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn gen_range(&mut self, min: u32, max: u32) -> u32 {
        let range = (max - min + 1) as u64;
        min + (self.next_u64() % range) as u32
    }
}

fn run_stress_test_program() -> std::io::Result<()> {
    let (t_cols, t_rows) = terminal::size().unwrap_or((100, 30));
    let cols = if t_cols == 0 { 100 } else { t_cols as usize };
    let rows = if t_rows == 0 { 30 } else { t_rows as usize };
    let seed = 0xEEA;

    let mut rng = SimpleRng::new(seed);
    let mut out = stdout();

    let render_frame = |rng: &mut SimpleRng, out: &mut std::io::Stdout| -> std::io::Result<()> {
        let mut buffer = String::new();
        // Reset, hide cursor, home
        buffer.push_str("\x1b[0m\x1b[?25l\x1b[H");
        for r in 1..=rows {
            buffer.push_str(&format!("\x1b[{r};1H"));
            for _c in 0..cols {
                let ch = char::from_u32(rng.gen_range(0x21, 0x7E)).unwrap_or(' ');
                let fg_r = rng.gen_range(0, 255);
                let fg_g = rng.gen_range(0, 255);
                let fg_b = rng.gen_range(0, 255);
                let bg_r = rng.gen_range(0, 255);
                let bg_g = rng.gen_range(0, 255);
                let bg_b = rng.gen_range(0, 255);
                
                let mut sgr = String::from("\x1b[0");
                if rng.gen_range(0, 1) == 1 { sgr.push_str(";1"); }
                if rng.gen_range(0, 1) == 1 { sgr.push_str(";3"); }
                if rng.gen_range(0, 1) == 1 { sgr.push_str(";4"); }
                if rng.gen_range(0, 1) == 1 { sgr.push_str(";7"); }
                sgr.push_str(&format!(";38;2;{fg_r};{fg_g};{fg_b};48;2;{bg_r};{bg_g};{bg_b}m"));
                
                buffer.push_str(&sgr);
                buffer.push(ch);
            }
        }
        buffer.push_str("\x1b[0m");
        out.write_all(buffer.as_bytes())?;
        out.flush()?;
        Ok(())
    };

    // Initial paint
    if let Err(e) = render_frame(&mut rng, &mut out) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e);
    }

    use std::io::IsTerminal;
    let is_tty = std::io::stdin().is_terminal();
    if is_tty {
        terminal::enable_raw_mode()?;
    }

    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 1];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if buf[0] == b'q' {
                    break;
                }
                if let Err(e) = render_frame(&mut rng, &mut out) {
                    if e.kind() == std::io::ErrorKind::BrokenPipe {
                        break;
                    }
                    if is_tty {
                        let _ = terminal::disable_raw_mode();
                    }
                    return Err(e);
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }

    if is_tty {
        terminal::disable_raw_mode()?;
    }
    
    // Restore cursor and clear SGR on exit so host shell isn't left in a weird state
    if let Err(e) = out.write_all(b"\x1b[0m\x1b[?25h\n") {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e);
        }
    } else {
        let _ = out.flush();
    }
    
    Ok(())
}
