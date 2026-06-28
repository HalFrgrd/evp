//! Translate parsed VHS [`KeySpec`]s into PTY byte sequences using
//! libghostty's key encoder.
//!
//! libghostty's encoder needs an [`Event`] populated with the logical key,
//! the modifier bitmask, and (for printable chars) the unmodified UTF‑8
//! text. It then handles all the platform / mode‑aware encoding for us
//! (cursor key application mode, modifyOtherKeys, kitty protocol, …).

use anyhow::Result;
use libghostty_vt::{
    Terminal,
    key::{self, Action, Encoder, Key, Mods},
};

use crate::script::{KeyAction, KeySpec, ModSet, NamedKey};

/// Wraps a libghostty [`Encoder`] tied to a specific [`Terminal`]'s modes.
///
/// We refresh the encoder options before every press so that mode changes
/// emitted by the running shell (DECCKM, kitty progressive enhancement, …)
/// are honoured.
pub struct KeyTranslator<'a> {
    encoder: Encoder<'a>,
    buf: Vec<u8>,
}

impl<'a> KeyTranslator<'a> {
    pub fn new() -> Result<Self> {
        Ok(Self {
            encoder: Encoder::new()?,
            buf: Vec::with_capacity(64),
        })
    }

    /// Encode one [`KeySpec`] into PTY bytes. The returned slice is owned by
    /// `self` and is overwritten on the next call.
    pub fn encode(
        &mut self,
        spec: &KeySpec,
        action: KeyAction,
        terminal: &Terminal<'_, '_>,
    ) -> Result<&[u8]> {
        self.encoder.set_options_from_terminal(terminal);

        let (lib_key, utf8) = lookup_key(spec.key);
        let mods = mods_to_lib(spec.mods);

        let mut event = key::Event::new()?;
        event
            .set_key(lib_key)
            .set_action(match action {
                KeyAction::Press => Action::Press,
                KeyAction::Release => Action::Release,
            })
            .set_mods(mods);

        // The encoder uses `utf8` only for printable characters; for named
        // keys (Enter, arrows, …) we leave it None so libghostty derives the
        // sequence from the logical key. We also drop the text when Ctrl is
        // held so libghostty can produce the correct C0 control byte
        // (e.g. Ctrl+C => 0x03) instead of the raw character.
        if let Some(text) = utf8 {
            if !spec.mods.ctrl {
                event.set_utf8(Some(text.to_string()));
            }
        } else if let NamedKey::Char(c) = spec.key {
            if !spec.mods.ctrl {
                event.set_utf8(Some(c.to_string()));
            }
        }
        if let NamedKey::Char(c) = spec.key {
            event.set_unshifted_codepoint(c.to_ascii_lowercase());
        }

        self.buf.clear();
        self.encoder.encode_to_vec(&event, &mut self.buf)?;
        Ok(&self.buf)
    }

    /// Encode a literal text string as raw UTF‑8 bytes – used by `Type`.
    ///
    /// We don't run individual characters through libghostty's key encoder
    /// because the application will gladly consume the raw bytes the user
    /// would have typed; this also keeps `Type` cheap.
    pub fn encode_text(&mut self, text: &str, _terminal: &Terminal<'_, '_>) -> Result<Vec<u8>> {
        Ok(text.as_bytes().to_vec())
    }
}

fn mods_to_lib(m: ModSet) -> Mods {
    let mut out = Mods::empty();
    if m.ctrl {
        out |= Mods::CTRL;
    }
    if m.alt {
        out |= Mods::ALT;
    }
    if m.shift {
        out |= Mods::SHIFT;
    }
    if m.super_key {
        out |= Mods::SUPER;
    }
    out
}

/// Map a [`NamedKey`] to a libghostty [`Key`] plus the printable text
/// (when applicable). The text is `None` for non‑printable keys.
fn lookup_key(k: NamedKey) -> (Key, Option<&'static str>) {
    match k {
        NamedKey::Enter => (Key::Enter, None),
        NamedKey::Escape => (Key::Escape, None),
        NamedKey::Tab => (Key::Tab, None),
        NamedKey::Backspace => (Key::Backspace, None),
        NamedKey::Delete => (Key::Delete, None),
        NamedKey::Insert => (Key::Insert, None),
        NamedKey::Space => (Key::Space, Some(" ")),
        NamedKey::Up => (Key::ArrowUp, None),
        NamedKey::Down => (Key::ArrowDown, None),
        NamedKey::Left => (Key::ArrowLeft, None),
        NamedKey::Right => (Key::ArrowRight, None),
        NamedKey::PageUp => (Key::PageUp, None),
        NamedKey::PageDown => (Key::PageDown, None),
        NamedKey::Home => (Key::Home, None),
        NamedKey::End => (Key::End, None),
        NamedKey::Shift => (Key::ShiftLeft, None),
        NamedKey::Control => (Key::ControlLeft, None),
        NamedKey::Alt => (Key::AltLeft, None),
        // libghostty exposes the platform "super/command/windows" modifier
        // as the Meta key family.
        NamedKey::Super => (Key::MetaLeft, None),
        // No native equivalent for ScrollUp/Down – fall back to PageUp/Down.
        NamedKey::ScrollUp => (Key::PageUp, None),
        NamedKey::ScrollDown => (Key::PageDown, None),
        NamedKey::Char(c) => (char_to_key(c), None),
    }
}

/// Map a single character to libghostty's logical key code. Only ASCII is
/// covered; unicode characters fall through to [`Key::Unidentified`] (the
/// caller should rely on the UTF‑8 text path for those).
#[allow(clippy::too_many_lines)]
fn char_to_key(c: char) -> Key {
    let lower = c.to_ascii_lowercase();
    match lower {
        'a' => Key::A,
        'b' => Key::B,
        'c' => Key::C,
        'd' => Key::D,
        'e' => Key::E,
        'f' => Key::F,
        'g' => Key::G,
        'h' => Key::H,
        'i' => Key::I,
        'j' => Key::J,
        'k' => Key::K,
        'l' => Key::L,
        'm' => Key::M,
        'n' => Key::N,
        'o' => Key::O,
        'p' => Key::P,
        'q' => Key::Q,
        'r' => Key::R,
        's' => Key::S,
        't' => Key::T,
        'u' => Key::U,
        'v' => Key::V,
        'w' => Key::W,
        'x' => Key::X,
        'y' => Key::Y,
        'z' => Key::Z,
        '0' | ')' => Key::Digit0,
        '1' | '!' => Key::Digit1,
        '2' | '@' => Key::Digit2,
        '3' | '#' => Key::Digit3,
        '4' | '$' => Key::Digit4,
        '5' | '%' => Key::Digit5,
        '6' | '^' => Key::Digit6,
        '7' | '&' => Key::Digit7,
        '8' | '*' => Key::Digit8,
        '9' | '(' => Key::Digit9,
        '-' | '_' => Key::Minus,
        '=' | '+' => Key::Equal,
        '[' | '{' => Key::BracketLeft,
        ']' | '}' => Key::BracketRight,
        '\\' | '|' => Key::Backslash,
        ';' | ':' => Key::Semicolon,
        '\'' | '"' => Key::Quote,
        ',' | '<' => Key::Comma,
        '.' | '>' => Key::Period,
        '/' | '?' => Key::Slash,
        '`' | '~' => Key::Backquote,
        ' ' => Key::Space,
        _ => Key::Unidentified,
    }
}
