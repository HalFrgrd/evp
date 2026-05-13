use std::io::{Read, Write, stdin, stdout};

use crossterm::{
    cursor,
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
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

    let mut input = stdin().lock();
    let mut seen = 0usize;
    while seen < max_keys {
        let Some(seq) = read_one_sequence(&mut input)? else {
            break;
        };
        let escaped = escape_bytes(&seq);
        let info = decode_key(&seq);
        writeln!(
            out,
            "key codepoint={} mods={} kind={} raw={}",
            info.codepoint, info.mods, info.kind, escaped
        )?;
        out.flush()?;
        seen += 1;
    }

    execute!(out, PopKeyboardEnhancementFlags)?;
    terminal::disable_raw_mode()
}

struct KeyInfo {
    codepoint: u32,
    mods: String,
    kind: &'static str,
}

fn decode_key(seq: &[u8]) -> KeyInfo {
    if seq == b"\r" || seq == b"\n" {
        return KeyInfo {
            codepoint: 13,
            mods: "NONE".to_string(),
            kind: "Press",
        };
    }

    if seq.len() >= 4 && seq[0] == 0x1b && seq[1] == b'[' && *seq.last().unwrap_or(&0) == b'u' {
        let body = std::str::from_utf8(&seq[2..seq.len() - 1]).unwrap_or_default();
        let mut parts = body.split(';');
        let codepoint = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_default();
        let mods_and_kind = parts.next().unwrap_or("1");
        let (mods_encoded, kind_encoded) = mods_and_kind
            .split_once(':')
            .map_or((mods_and_kind, "1"), |(m, k)| (m, k));
        let mods_bits = mods_encoded
            .parse::<u32>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .unwrap_or(0);
        return KeyInfo {
            codepoint,
            mods: mods_to_string(mods_bits),
            kind: match kind_encoded {
                "2" => "Repeat",
                "3" => "Release",
                _ => "Press",
            },
        };
    }

    let codepoint = seq.first().copied().map(u32::from).unwrap_or_default();
    KeyInfo {
        codepoint,
        mods: "NONE".to_string(),
        kind: "Press",
    }
}

fn mods_to_string(bits: u32) -> String {
    let mut mods = Vec::new();
    if bits & 0b0001 != 0 {
        mods.push("SHIFT");
    }
    if bits & 0b0010 != 0 {
        mods.push("ALT");
    }
    if bits & 0b0100 != 0 {
        mods.push("CTRL");
    }
    if bits & 0b1000 != 0 {
        mods.push("SUPER");
    }
    if mods.is_empty() {
        "NONE".to_string()
    } else {
        mods.join("|")
    }
}

fn read_one_sequence(input: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut first = [0u8; 1];
    match input.read(&mut first)? {
        0 => Ok(None),
        _ => {
            if first[0] != 0x1b {
                return Ok(Some(vec![first[0]]));
            }
            let mut seq = vec![first[0]];
            let mut next = [0u8; 1];
            loop {
                match input.read(&mut next)? {
                    0 => return Ok(Some(seq)),
                    _ => {
                        seq.push(next[0]);
                        if (0x40..=0x7e).contains(&next[0]) && seq.len() >= 3 {
                            return Ok(Some(seq));
                        }
                    }
                }
            }
        }
    }
}

fn escape_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| match b {
            b'\x1b' => "\\x1b".to_string(),
            b'\r' => "\\r".to_string(),
            b'\n' => "\\n".to_string(),
            0x20..=0x7e => (*b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect::<Vec<_>>()
        .join("")
}
