//! VHS `.tape` script tokenizer + parser.
//!
//! The grammar is line-oriented and small enough that a hand-rolled parser
//! is much simpler than dragging in `nom`/`pest`. We split each non-blank
//! line into tokens (whitespace separated, with `"…"` / `'…'` / `` `…` ``
//! quoted strings preserved verbatim) then dispatch on the first token.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use super::ast::{Event, KeySpec, ModSet, NamedKey, Script, Settings, WaitScope};
use crate::style::{Theme, WindowBarStyle, parse_hex_color};

/// Parse a complete script source.
///
/// `Source` directives are resolved relative to the current working
/// directory (since we don't know the source file's location).
pub fn parse(src: &str) -> Result<Script> {
    let mut script = Script::default();
    let mut visited = HashSet::new();
    parse_into(src, None, &mut script, &mut visited)?;
    Ok(script)
}

/// Parse a `.tape` file from disk. `Source` directives are resolved
/// relative to the file's parent directory (matching vhs's behaviour).
pub fn parse_path(path: &Path) -> Result<Script> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let src = std::fs::read_to_string(&canonical)
        .with_context(|| format!("reading {}", canonical.display()))?;
    let mut script = Script::default();
    let mut visited = HashSet::new();
    visited.insert(canonical.clone());
    let base = canonical.parent().map(Path::to_path_buf);
    parse_into(&src, base.as_deref(), &mut script, &mut visited)?;
    Ok(script)
}

/// Internal: append `src`'s parse result onto `script`. `base_dir` is the
/// directory used to resolve relative `Source` paths; `None` falls back
/// to the process cwd. `visited` is the set of canonical paths already
/// being parsed, used to break include cycles.
fn parse_into(
    src: &str,
    base_dir: Option<&Path>,
    script: &mut Script,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    for (lineno, raw) in src.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        parse_line(line, script, base_dir, visited)
            .with_context(|| format!("line {}: `{}`", lineno + 1, raw))?;
    }
    Ok(())
}

fn strip_comment(s: &str) -> &str {
    // Comments are `#` to end-of-line, but only outside of quoted strings.
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' | b'`' => quote = Some(b),
                b'#' => return &s[..i],
                _ => {}
            },
        }
    }
    s
}

/// Tokenize a single line. Quoted strings keep their delimiters so the
/// caller can tell `"foo"` apart from the bare identifier `foo`.
fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            let quote = c;
            chars.next();
            let mut s = String::from(quote);
            let mut closed = false;
            while let Some(c) = chars.next() {
                s.push(c);
                if c == quote {
                    closed = true;
                    break;
                }
                // Only `"…"` and `'…'` honour `\<quote>` as an escape so
                // the user can embed the quote character itself. Other
                // escape sequences (`\e`, `\n`, `\\`, …) are passed
                // through verbatim and interpreted downstream by the
                // event executor. Backticks are raw strings — `\` has no
                // special meaning at all — which is what VHS does and is
                // why tapes can write things like
                //   Type `flyline … --fps 3 \`
                // without the trailing `\` swallowing the closing
                // backtick.
                if quote != '`'
                    && c == '\\'
                    && let Some(&next) = chars.peek()
                    && next == quote
                {
                    s.push(next);
                    chars.next();
                }
            }
            if !closed {
                bail!("unterminated string literal");
            }
            out.push(s);
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                s.push(c);
                chars.next();
            }
            out.push(s);
        }
    }
    Ok(out)
}

fn parse_line(
    line: &str,
    script: &mut Script,
    base_dir: Option<&Path>,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    let tokens = tokenize(line)?;
    if tokens.is_empty() {
        return Ok(());
    }
    let head = tokens[0].as_str();
    let rest = &tokens[1..];

    match head {
        "Output" => {
            let path = rest
                .first()
                .map(|t| unquote(t).to_string())
                .ok_or_else(|| anyhow!("Output requires a path"))?;
            if !script.outputs.is_empty() {
                bail!(
                    "evp only supports a single `Output` directive per tape (got `{}` after `{}`). \
                     VHS allows multiple outputs; this is tracked under \"VHS feature parity\" in the README.",
                    path,
                    script.outputs[0]
                );
            }
            // Restrict output extensions up-front so users see the failure
            // as soon as the tape is parsed rather than at render time.
            let ext = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            match ext.as_deref() {
                Some("gif") | Some("svg") => {}
                Some(other) => bail!(
                    "Output `{path}` has unsupported extension `.{other}`. \
                     evp currently writes `.gif` or `.svg` only. \
                     `.mp4`, `.webm`, `.png` (frame directory), `.txt`, and `.ascii` outputs \
                     are VHS features evp does not implement yet \
                     (see README \"VHS feature parity\")."
                ),
                None => bail!(
                    "Output `{path}` has no extension. evp picks the renderer from the extension; \
                     use `.gif` or `.svg`."
                ),
            }
            script.outputs.push(path);
        }
        "Set" => apply_set(rest, &mut script.settings)?,
        "Env" => {
            let (k, v) = (
                rest.first().ok_or_else(|| anyhow!("Env KEY missing"))?,
                rest.get(1).ok_or_else(|| anyhow!("Env value missing"))?,
            );
            script
                .env
                .push((unquote(k).to_string(), unquote(v).to_string()));
        }
        "Require" => {
            for t in rest {
                script.require.push(unquote(t).to_string());
            }
        }
        "Sleep" => {
            let d = rest
                .first()
                .ok_or_else(|| anyhow!("Sleep needs a duration"))?;
            script.events.push(Event::Sleep(parse_duration(d)?));
        }
        "Hide" => script.events.push(Event::Hide),
        "Show" => script.events.push(Event::Show),
        "Screenshot" => {
            let path = rest
                .first()
                .map(|t| unquote(t).to_string())
                .ok_or_else(|| anyhow!("Screenshot needs a path"))?;
            if !path.ends_with(".png") {
                bail!("Screenshot expects a .png path");
            }
            script.events.push(Event::Screenshot(path));
        }
        "Wait" => script.events.push(parse_wait(rest, &script.settings)?),
        "Source" => {
            // `Source path/to/other.tape` — inline the contents at this
            // point. Equivalent to a textual #include: events, settings,
            // env, etc. all merge into the current script as if the
            // sourced file's lines were pasted here.
            let raw = rest
                .first()
                .ok_or_else(|| anyhow!("Source requires a path"))?;
            let rel = unquote(raw);
            let resolved = match base_dir {
                Some(d) => d.join(rel),
                None => PathBuf::from(rel),
            };
            let canonical = resolved
                .canonicalize()
                .with_context(|| format!("resolving Source `{}`", rel))?;
            if !visited.insert(canonical.clone()) {
                bail!("Source cycle detected including `{}`", canonical.display());
            }
            let inner = std::fs::read_to_string(&canonical)
                .with_context(|| format!("reading {}", canonical.display()))?;
            let inner_base = canonical.parent().map(Path::to_path_buf);
            let res = parse_into(&inner, inner_base.as_deref(), script, visited);
            visited.remove(&canonical);
            res?;
        }
        "Copy" => {
            if rest.is_empty() {
                bail!("Copy requires at least one quoted string");
            }
            let mut text = String::new();
            for (idx, token) in rest.iter().enumerate() {
                if idx > 0 {
                    text.push(' ');
                }
                text.push_str(unquote_required(token)?);
            }
            script.events.push(Event::Copy(text));
        }
        "Paste" => script.events.push(Event::Paste),
        // `Type[@duration] "text" ["text" ...]`
        h if h == "Type" || h.starts_with("Type@") => {
            let delay = parse_at_duration(h, "Type", script.settings.typing_speed)?;
            if rest.is_empty() {
                bail!("Type requires at least one quoted string");
            }
            for t in rest {
                let text = unquote_required(t)?;
                script.events.push(Event::Type {
                    text: text.to_string(),
                    delay,
                });
            }
        }
        // Anything else is treated as a key press, possibly with `@delay`
        // and a trailing repeat count.
        _ => script.events.push(parse_key_event(head, rest)?),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Set
// ---------------------------------------------------------------------------

fn apply_set(rest: &[String], s: &mut Settings) -> Result<()> {
    let key = rest
        .first()
        .ok_or_else(|| anyhow!("Set requires a key"))?
        .as_str();
    match key {
        "Shell" => s.shell = Some(set_scalar(rest, key)?),
        "FontFamily" => s.font_family = Some(set_scalar(rest, key)?),
        "FontSize" => s.font_size = set_scalar(rest, key)?.parse()?,
        "Width" => s.width = set_scalar(rest, key)?.parse()?,
        "Height" => s.height = set_scalar(rest, key)?.parse()?,
        // vhs has no native cell-grid setting; we expose them for convenience.
        "Cols" | "Columns" => s.cols = Some(set_scalar(rest, key)?.parse()?),
        "Rows" => s.rows = Some(set_scalar(rest, key)?.parse()?),
        "Padding" => s.padding = set_scalar(rest, key)?.parse()?,
        "Margin" => s.margin = set_scalar(rest, key)?.parse()?,
        "MarginFill" => s.margin_fill = parse_hex_color(&set_scalar(rest, key)?)?,
        "WindowBar" => s.window_bar = WindowBarStyle::parse(&set_scalar(rest, key)?)?,
        "WindowBarSize" => s.window_bar_size = set_scalar(rest, key)?.parse()?,
        "BorderRadius" => s.border_radius = set_scalar(rest, key)?.parse()?,
        "LineHeight" => s.line_height = set_scalar(rest, key)?.parse()?,
        "Framerate" | "FrameRate" | "FPS" => s.framerate = set_scalar(rest, key)?.parse()?,
        "PlaybackSpeed" => s.playback_speed = set_scalar(rest, key)?.parse()?,
        "TypingSpeed" => s.typing_speed = parse_duration(&set_scalar(rest, key)?)?,
        "WaitTimeout" => s.wait_timeout = parse_duration(&set_scalar(rest, key)?)?,
        "WaitPattern" => s.wait_pattern = set_scalar(rest, key)?,
        // VHS settings that evp does NOT yet implement. We bail loudly so a
        // tape author isn't misled into thinking these are taking effect.
        // See README ("VHS feature parity") for the up-to-date matrix.
        "Theme" => s.theme = Theme::from_spec(&set_value(rest, key)?)?,
        "LetterSpacing" => bail!(unsupported_set_msg("LetterSpacing")),
        "CursorBlink" => s.cursor_blink = set_scalar(rest, key)?.parse()?,
        "LoopOffset" => bail!(unsupported_set_msg("LoopOffset")),
        other => bail!("unknown Set key: {other}"),
    }
    Ok(())
}

/// Build a consistent error message for `Set` keys VHS understands but evp
/// does not implement yet. Surfaced both in the parser error and in the
/// README's "VHS feature parity" table.
fn unsupported_set_msg(key: &str) -> String {
    format!(
        "`Set {key}` is a VHS feature that evp does not implement yet. \
         See the README's \"VHS feature parity\" section. \
         Remove the directive or run the tape with vhs instead."
    )
}

fn set_scalar(rest: &[String], key: &str) -> Result<String> {
    rest.get(1)
        .map(|t| unquote(t).to_string())
        .ok_or_else(|| anyhow!("Set {key} requires a value"))
}

fn set_value(rest: &[String], key: &str) -> Result<String> {
    if rest.len() < 2 {
        bail!("Set {key} requires a value");
    }
    if rest.len() == 2 {
        return Ok(unquote(&rest[1]).to_string());
    }
    Ok(rest[1..].join(" "))
}

// ---------------------------------------------------------------------------
// Wait
// ---------------------------------------------------------------------------

fn parse_wait(tokens: &[String], settings: &Settings) -> Result<Event> {
    let mut scope = WaitScope::Line;
    let mut timeout = settings.wait_timeout;
    let mut pattern = settings.wait_pattern.clone();
    for tok in tokens {
        match tok.as_str() {
            "Line" => scope = WaitScope::Line,
            "Screen" => scope = WaitScope::Screen,
            t if let Some(d) = t.strip_prefix('@') => timeout = parse_duration(d)?,
            t if t.starts_with('/') && t.ends_with('/') && t.len() >= 2 => {
                pattern = t[1..t.len() - 1].to_string();
            }
            other => bail!("unexpected token in Wait: `{other}`"),
        }
    }
    Ok(Event::Wait {
        scope,
        timeout,
        pattern,
    })
}

// ---------------------------------------------------------------------------
// Key event parsing (`Enter`, `Ctrl+C`, `Down@50ms 5`, …)
// ---------------------------------------------------------------------------

fn parse_key_event(head: &str, rest: &[String]) -> Result<Event> {
    // Split optional `@duration` off the head.
    let (name_part, delay) = match head.split_once('@') {
        Some((name, dur)) => (name, parse_duration(dur)?),
        None => (head, Duration::ZERO),
    };
    let key = parse_key_spec(name_part)?;

    // Trailing token (if any) is the repeat count.
    let count = match rest.first() {
        Some(t) => t
            .parse::<u32>()
            .with_context(|| format!("invalid repeat count `{t}`"))?,
        None => 1,
    };
    Ok(Event::Key { key, count, delay })
}

/// Parse `Ctrl+Shift+Tab` / `Alt+x` / `Enter` into a [`KeySpec`].
fn parse_key_spec(s: &str) -> Result<KeySpec> {
    let mut mods = ModSet::default();
    let mut last = "";
    for part in s.split('+') {
        match part {
            "Ctrl" | "Control" => mods.ctrl = true,
            "Alt" | "Option" => mods.alt = true,
            "Shift" => mods.shift = true,
            _ => last = part,
        }
    }
    if last.is_empty() {
        bail!("missing key name in `{s}`");
    }
    let key = match last {
        "Enter" | "Return" => NamedKey::Enter,
        "Escape" | "Esc" => NamedKey::Escape,
        "Tab" => NamedKey::Tab,
        "Backspace" => NamedKey::Backspace,
        "Delete" => NamedKey::Delete,
        "Insert" => NamedKey::Insert,
        "Space" => NamedKey::Space,
        "Up" => NamedKey::Up,
        "Down" => NamedKey::Down,
        "Left" => NamedKey::Left,
        "Right" => NamedKey::Right,
        "PageUp" => NamedKey::PageUp,
        "PageDown" => NamedKey::PageDown,
        "Home" => NamedKey::Home,
        "End" => NamedKey::End,
        "ScrollUp" => NamedKey::ScrollUp,
        "ScrollDown" => NamedKey::ScrollDown,
        // Single character (e.g. `C` in `Ctrl+C`). We keep the character
        // verbatim – translation to the right libghostty `Key` happens in
        // the `keys` module.
        other => {
            let mut chars = other.chars();
            let c = chars.next().ok_or_else(|| anyhow!("empty key in `{s}`"))?;
            if chars.next().is_some() {
                bail!("unknown key name `{other}`");
            }
            NamedKey::Char(c)
        }
    };
    Ok(KeySpec { key, mods })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_at_duration(head: &str, prefix: &str, default: Duration) -> Result<Duration> {
    match head.strip_prefix(prefix).and_then(|s| s.strip_prefix('@')) {
        Some(d) => parse_duration(d),
        None => Ok(default),
    }
}

/// Parse vhs durations: `1s`, `500ms`, `2m`, `0.5s`. Bare numbers are seconds.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num_str, unit) = if let Some(rest) = s.strip_suffix("ms") {
        (rest, "ms")
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, "s")
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, "m")
    } else {
        (s, "s")
    };
    let n: f64 = num_str
        .parse()
        .with_context(|| format!("invalid duration `{s}`"))?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        _ => unreachable!(),
    };
    Ok(Duration::from_secs_f64(secs))
}

/// Strip surrounding quotes if present (no escape processing).
fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = *bytes.last().unwrap();
        if first == last && matches!(first, b'"' | b'\'' | b'`') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn unquote_required(s: &str) -> Result<&str> {
    let bytes = s.as_bytes();
    if bytes.len() < 2
        || bytes[0] != *bytes.last().unwrap()
        || !matches!(bytes[0], b'"' | b'\'' | b'`')
    {
        bail!("expected quoted string, got `{s}`");
    }
    Ok(&s[1..s.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_script() {
        let src = r#"
            # leading comment
            Output out.gif
            Set FontSize 18
            Set TypingSpeed 30ms
            Type "hello"
            Sleep 500ms
            Enter
            Ctrl+C
            Down@20ms 5
        "#;
        let s = parse(src).unwrap();
        assert_eq!(s.outputs, vec!["out.gif"]);
        assert_eq!(s.settings.font_size, 18.0);
        assert_eq!(s.events.len(), 5);
        match &s.events[0] {
            Event::Type { text, delay } => {
                assert_eq!(text, "hello");
                assert_eq!(*delay, Duration::from_millis(30));
            }
            _ => panic!(),
        }
        match &s.events[3] {
            Event::Key { key, .. } => {
                assert!(key.mods.ctrl);
                assert_eq!(key.key, NamedKey::Char('C'));
            }
            _ => panic!(),
        }
        match &s.events[4] {
            Event::Key { key, count, delay } => {
                assert_eq!(key.key, NamedKey::Down);
                assert_eq!(*count, 5);
                assert_eq!(*delay, Duration::from_millis(20));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn source_inlines_another_tape_relative_to_file() {
        // Layout:
        //   <tmp>/main.tape   ->  Source helpers/inner.tape
        //   <tmp>/helpers/inner.tape  ->  Type "hello"
        let dir = tempdir();
        std::fs::create_dir(dir.join("helpers")).unwrap();
        std::fs::write(
            dir.join("helpers/inner.tape"),
            "Type \"hello\"\nSleep 100ms\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.tape"),
            "Output out.gif\nSource helpers/inner.tape\nEnter\n",
        )
        .unwrap();

        let s = parse_path(&dir.join("main.tape")).unwrap();
        assert_eq!(s.outputs, vec!["out.gif"]);
        assert_eq!(s.events.len(), 3);
        assert!(matches!(&s.events[0], Event::Type { text, .. } if text == "hello"));
        assert!(matches!(&s.events[1], Event::Sleep(_)));
        assert!(matches!(&s.events[2], Event::Key { .. }));
    }

    #[test]
    fn source_cycle_is_rejected() {
        let dir = tempdir();
        std::fs::write(dir.join("a.tape"), "Source b.tape\n").unwrap();
        std::fs::write(dir.join("b.tape"), "Source a.tape\n").unwrap();
        let err = parse_path(&dir.join("a.tape")).unwrap_err();
        assert!(
            format!("{err:#}").contains("cycle"),
            "expected cycle error, got: {err:#}"
        );
    }

    #[test]
    fn unsupported_set_keys_bail() {
        for key in ["LetterSpacing", "LoopOffset"] {
            let src = format!("Output out.gif\nSet {key} something\n");
            let err = parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains(key) && msg.contains("VHS feature"),
                "expected `{key}` to bail with parity error, got: {msg}"
            );
        }
    }

    #[test]
    fn copy_paste_parse() {
        let script = parse("Output out.gif\nCopy \"hello\"\nPaste\n").unwrap();
        assert!(matches!(&script.events[0], Event::Copy(text) if text == "hello"));
        assert!(matches!(&script.events[1], Event::Paste));
    }

    #[test]
    fn multiple_outputs_bail() {
        let err = parse("Output a.gif\nOutput b.gif\n").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("single `Output`"),
            "expected single-output error, got: {msg}"
        );
    }

    #[test]
    fn unsupported_output_extensions_bail() {
        for ext in ["mp4", "webm", "txt", "ascii", "png"] {
            let err = parse(&format!("Output out.{ext}\n")).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(ext), "expected `.{ext}` to bail, got: {msg}");
        }
    }

    #[test]
    fn screenshot_parses() {
        let s = parse("Output out.gif\nScreenshot shot.png\n").unwrap();
        assert!(matches!(&s.events[0], Event::Screenshot(p) if p == "shot.png"));
    }

    #[test]
    fn theme_json_parses() {
        let s = parse(
            "Output out.gif\nSet Theme { \"name\": \"Whimsy\", \"background\": \"#29283b\", \"foreground\": \"#b3b0d6\" }\n",
        )
        .unwrap();
        assert_eq!(s.settings.theme.name.as_deref(), Some("Whimsy"));
        assert_eq!(s.settings.theme.background, "#29283b");
    }

    #[test]
    fn theme_preset_parses() {
        let s = parse("Output out.gif\nSet Theme \"Whimsy\"\n").unwrap();
        assert_eq!(s.settings.theme.name.as_deref(), Some("Whimsy"));
    }

    /// Tiny self-cleaning tempdir helper so we don't pull in `tempfile`.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        for attempt in 0u32..1000 {
            let mut p = base.clone();
            p.push(format!(
                "evp-parser-test-{}-{}-{}",
                std::process::id(),
                stamp,
                attempt
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => return p,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp dir {}: {e}", p.display()),
            }
        }

        panic!("failed to create unique temp dir after 1000 attempts")
    }
}
