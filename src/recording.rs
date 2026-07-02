//! Recording artifact: a sequence of terminal frames captured at a fixed
//! framerate.
//!
//! The runner produces dense [`RawFrame`]s at the target framerate. Consumers
//! that need the intermediate format fold those raw frames into a [`Recording`]
//! which keeps the first frame in full and subsequent frames as **cell diffs**
//! against the prior one. This dramatically shrinks JSON serialisation while
//! still being a lossless representation of the captured terminal state.

use serde::{Deserialize, Serialize};

use crate::{FrameStyle, render_common::ViewportConfig};

/// A single colored cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellSnap {
    /// UTF‑8 grapheme cluster sitting in this cell. Empty string means the
    /// cell was either blank or is a continuation of a wide character to
    /// its left (a wide char occupies two cells; the second is empty here).
    pub text: String,
    /// Foreground RGB.
    pub fg: [u8; 3],
    /// Background RGB.
    pub bg: [u8; 3],
    /// Packed style flags – see [`StyleFlags`].
    pub flags: u8,
}

/// Bit positions for [`CellSnap::flags`].
pub mod style_flags {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    pub const INVERSE: u8 = 1 << 3;
    pub const STRIKETHROUGH: u8 = 1 << 4;
    /// SGR 2 – faint/dim: foreground blended 50% toward the background.
    pub const DIM: u8 = 1 << 5;
}

impl CellSnap {
    pub fn blank(default_fg: [u8; 3], default_bg: [u8; 3]) -> Self {
        Self {
            text: String::new(),
            fg: default_fg,
            bg: default_bg,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseState {
    Moving,
    Clicking,
    Dragging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerShape {
    #[default]
    Default,
    Text,
    Pointer,
    Grabbing,
}

impl PointerShape {
    pub fn as_str(&self) -> &'static str {
        match self {
            PointerShape::Default => "default",
            PointerShape::Text => "text",
            PointerShape::Pointer => "pointer",
            PointerShape::Grabbing => "grabbing",
        }
    }
}

impl std::str::FromStr for PointerShape {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "default" => PointerShape::Default,
            "text" => PointerShape::Text,
            "pointer" => PointerShape::Pointer,
            "grabbing" => PointerShape::Grabbing,
            _ => PointerShape::Default,
        })
    }
}

impl<'de> Deserialize<'de> for PointerShape {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(s.parse().unwrap_or(PointerShape::Default))
    }
}

impl Serialize for PointerShape {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Osc22Parser {
    state: ParserState,
    buffer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ParserState {
    #[default]
    Ground,
    Escape,
    Osc,
    IgnoreOsc,
    IgnoreEsc,
    Osc22Value,
    EscSt,
}

impl Osc22Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, byte: u8) -> Option<PointerShape> {
        match self.state {
            ParserState::Ground => {
                if byte == 0x1b {
                    self.state = ParserState::Escape;
                } else if byte == 0x9d {
                    self.state = ParserState::Osc;
                    self.buffer.clear();
                }
            }
            ParserState::Escape => {
                if byte == b']' {
                    self.state = ParserState::Osc;
                    self.buffer.clear();
                } else {
                    self.state = ParserState::Ground;
                }
            }
            ParserState::Osc => {
                if byte.is_ascii_digit() {
                    self.buffer.push(byte as char);
                } else if byte == b';' {
                    if self.buffer == "22" {
                        self.state = ParserState::Osc22Value;
                    } else {
                        self.state = ParserState::IgnoreOsc;
                    }
                    self.buffer.clear();
                } else if byte == 0x07 {
                    self.state = ParserState::Ground;
                } else if byte == 0x1b {
                    self.state = ParserState::Escape;
                } else {
                    self.state = ParserState::IgnoreOsc;
                }
            }
            ParserState::IgnoreOsc => {
                if byte == 0x07 {
                    self.state = ParserState::Ground;
                } else if byte == 0x1b {
                    self.state = ParserState::IgnoreEsc;
                }
            }
            ParserState::IgnoreEsc => {
                if byte == b'\\' {
                    self.state = ParserState::Ground;
                } else {
                    self.state = ParserState::IgnoreOsc;
                }
            }
            ParserState::Osc22Value => {
                if byte == 0x07 {
                    let shape = self.buffer.parse::<PointerShape>().ok();
                    self.state = ParserState::Ground;
                    self.buffer.clear();
                    return shape;
                } else if byte == 0x1b {
                    self.state = ParserState::EscSt;
                } else {
                    if self.buffer.len() < 64 {
                        self.buffer.push(byte as char);
                    }
                }
            }
            ParserState::EscSt => {
                if byte == b'\\' {
                    let shape = self.buffer.parse::<PointerShape>().ok();
                    self.state = ParserState::Ground;
                    self.buffer.clear();
                    return shape;
                } else {
                    self.state = ParserState::Ground;
                }
            }
        }
        None
    }
}

/// A complete terminal grid plus cursor state, captured at a single point
/// in time. Produced by the runner thread and optionally shipped over channels
/// to raw-frame consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFrame {
    /// Time relative to recording start, in milliseconds.
    pub t_ms: u32,
    pub cols: u16,
    pub rows: u16,
    /// `cols * rows` cells in row-major order.
    pub cells: Vec<CellSnap>,
    /// Cursor (col, row) in viewport coordinates. `None` when hidden.
    pub cursor: Option<(u16, u16)>,
    /// Terminal default colors (fg, bg) at this point in time.
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    #[serde(default)]
    pub cursor_color: Option<[u8; 3]>,
    #[serde(default)]
    pub cursor_accent: Option<[u8; 3]>,
    #[serde(default)]
    pub mouse_cursor: Option<(f32, f32, MouseState)>,
    #[serde(default)]
    pub pointer_shape: PointerShape,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,
}

impl RawFrame {
    pub fn is_visually_identical(&self, other: &RawFrame) -> bool {
        self.cols == other.cols
            && self.rows == other.rows
            && self.cursor == other.cursor
            && self.default_fg == other.default_fg
            && self.default_bg == other.default_bg
            && self.cursor_color == other.cursor_color
            && self.cursor_accent == other.cursor_accent
            && self.title == other.title
            && self.mouse_cursor == other.mouse_cursor
            && self.pointer_shape == other.pointer_shape
            && self.cells == other.cells
    }
}

/// A single cell diff entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellChange {
    /// Linear cell index `row * cols + col`.
    pub idx: u32,
    pub cell: CellSnap,
}

/// A frame in a [`Recording`]. Either a full keyframe or a list of changed
/// cells against the previous frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Key {
        t_ms: u32,
        cursor: Option<(u16, u16)>,
        default_fg: [u8; 3],
        default_bg: [u8; 3],
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cursor_color: Option<[u8; 3]>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cursor_accent: Option<[u8; 3]>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        mouse_cursor: Option<(f32, f32, MouseState)>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pointer_shape: Option<PointerShape>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        title: Option<String>,
        cells: Vec<CellSnap>,
    },
    Diff {
        t_ms: u32,
        cursor: Option<(u16, u16)>,
        default_fg: [u8; 3],
        default_bg: [u8; 3],
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cursor_color: Option<[u8; 3]>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cursor_accent: Option<[u8; 3]>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        mouse_cursor: Option<(f32, f32, MouseState)>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pointer_shape: Option<PointerShape>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        title: Option<String>,
        changes: Vec<CellChange>,
    },
}

impl Frame {
    pub fn title(&self) -> Option<&str> {
        match self {
            Frame::Key { title, .. } | Frame::Diff { title, .. } => title.as_deref(),
        }
    }

    pub fn t_ms(&self) -> u32 {
        match self {
            Frame::Key { t_ms, .. } | Frame::Diff { t_ms, .. } => *t_ms,
        }
    }

    /// Cursor position recorded with this frame, if visible.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        match self {
            Frame::Key { cursor, .. } | Frame::Diff { cursor, .. } => *cursor,
        }
    }

    pub fn cursor_color(&self) -> Option<[u8; 3]> {
        match self {
            Frame::Key { cursor_color, .. } | Frame::Diff { cursor_color, .. } => *cursor_color,
        }
    }

    pub fn cursor_accent(&self) -> Option<[u8; 3]> {
        match self {
            Frame::Key { cursor_accent, .. } | Frame::Diff { cursor_accent, .. } => *cursor_accent,
        }
    }

    pub fn mouse_cursor(&self) -> Option<(f32, f32, MouseState)> {
        match self {
            Frame::Key { mouse_cursor, .. } | Frame::Diff { mouse_cursor, .. } => *mouse_cursor,
        }
    }

    pub fn pointer_shape(&self) -> PointerShape {
        match self {
            Frame::Key { pointer_shape, .. } | Frame::Diff { pointer_shape, .. } => {
                pointer_shape.unwrap_or(PointerShape::Default)
            }
        }
    }
}

/// The full recording artifact (de)serialisable to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    /// CSS em-square size used when the GIF renderer measured font metrics.
    /// Zero for recordings produced before this field was introduced.
    #[serde(default)]
    pub font_size_px: f32,
    /// Raw font bbox height (ascent − descent) at `font_size_px`, without the
    /// `line_height` multiplier.  Zero when unavailable.
    #[serde(default)]
    pub char_height_px: u32,
    /// Raw scaled font ascent at `font_size_px` (before centering).
    /// Zero when unavailable.
    #[serde(default)]
    pub ascent_px: u32,
    /// Additive pixels added to each cell's width.
    #[serde(default)]
    pub letter_spacing: f32,
    pub frame_style: FrameStyle,
    pub frames: Vec<Frame>,
}

/// Configuration constants used while folding dense [`RawFrame`]s into a
/// diff-compressed [`Recording`].
#[derive(Debug, Clone, Copy)]
pub struct RecordingConfig {
    pub viewport: ViewportConfig,
    pub keyframe_interval: u32,
}

/// Incrementally folds dense [`RawFrame`]s into a diff-compressed
/// [`Recording`].
pub struct RecordingBuilder {
    cfg: RecordingConfig,
    frames: Vec<Frame>,
    last_dense: Option<RawFrame>,
    frames_since_key: u32,
}

impl RecordingBuilder {
    pub fn new(cfg: RecordingConfig) -> Self {
        Self {
            cfg,
            frames: Vec::new(),
            last_dense: None,
            frames_since_key: 0,
        }
    }

    pub fn push_raw(&mut self, frame: RawFrame) {
        let is_key = match &self.last_dense {
            None => true,
            Some(prev) => {
                prev.cols != frame.cols
                    || prev.rows != frame.rows
                    || self.frames_since_key >= self.cfg.keyframe_interval
            }
        };

        if is_key {
            self.frames.push(Frame::Key {
                t_ms: frame.t_ms,
                cursor: frame.cursor,
                default_fg: frame.default_fg,
                default_bg: frame.default_bg,
                cursor_color: frame.cursor_color,
                cursor_accent: frame.cursor_accent,
                mouse_cursor: frame.mouse_cursor,
                pointer_shape: if frame.pointer_shape == PointerShape::Default {
                    None
                } else {
                    Some(frame.pointer_shape)
                },
                title: frame.title.clone(),
                cells: frame.cells.clone(),
            });
            self.frames_since_key = 0;
        } else {
            let prev = self.last_dense.as_ref().unwrap();
            let mut changes: Vec<CellChange> = Vec::new();
            for (idx, (a, b)) in prev.cells.iter().zip(frame.cells.iter()).enumerate() {
                if a != b {
                    changes.push(CellChange {
                        idx: idx as u32,
                        cell: b.clone(),
                    });
                }
            }
            self.frames.push(Frame::Diff {
                t_ms: frame.t_ms,
                cursor: frame.cursor,
                default_fg: frame.default_fg,
                default_bg: frame.default_bg,
                cursor_color: frame.cursor_color,
                cursor_accent: frame.cursor_accent,
                mouse_cursor: frame.mouse_cursor,
                pointer_shape: if frame.pointer_shape == PointerShape::Default {
                    None
                } else {
                    Some(frame.pointer_shape)
                },
                title: frame.title.clone(),
                changes,
            });
            self.frames_since_key += 1;
        }

        self.last_dense = Some(frame);
    }

    pub fn finish(self) -> Recording {
        Recording {
            cols: self.cfg.viewport.cols,
            rows: self.cfg.viewport.rows,
            framerate: self.cfg.viewport.framerate,
            cell_width_px: self.cfg.viewport.cell_width_px,
            cell_height_px: self.cfg.viewport.cell_height_px,
            font_size_px: self.cfg.viewport.font_size_px,
            char_height_px: self.cfg.viewport.char_height_px,
            ascent_px: self.cfg.viewport.ascent_px,
            letter_spacing: self.cfg.viewport.letter_spacing,
            frame_style: self.cfg.viewport.frame_style,
            frames: self.frames,
        }
    }
}

impl Recording {
    /// Reconstruct a dense [`RawFrame`] for `index` by replaying diffs from
    /// the most recent keyframe at or before it. Used by the renderer.
    pub fn reconstruct(&self, index: usize) -> Option<RawFrame> {
        let total = (self.cols as usize) * (self.rows as usize);
        // Find most recent keyframe.
        let mut keyframe_idx = None;
        for i in (0..=index).rev() {
            if matches!(self.frames.get(i)?, Frame::Key { .. }) {
                keyframe_idx = Some(i);
                break;
            }
        }
        let key_i = keyframe_idx?;
        let mut cells: Vec<CellSnap>;
        let mut t_ms;
        let mut cursor;
        let mut default_fg;
        let mut default_bg;
        let mut cursor_color;
        let mut cursor_accent;
        let mut mouse_cursor;
        let mut pointer_shape;
        let mut title;
        match &self.frames[key_i] {
            Frame::Key {
                t_ms: t,
                cursor: c,
                default_fg: dfg,
                default_bg: dbg,
                cursor_color: cc,
                cursor_accent: ca,
                mouse_cursor: mc,
                pointer_shape: ps,
                title: ttl,
                cells: cs,
            } => {
                cells = cs.clone();
                t_ms = *t;
                cursor = *c;
                default_fg = *dfg;
                default_bg = *dbg;
                cursor_color = *cc;
                cursor_accent = *ca;
                mouse_cursor = *mc;
                pointer_shape = ps.unwrap_or(PointerShape::Default);
                title = ttl.clone();
            }
            Frame::Diff { .. } => unreachable!(),
        }
        if cells.len() != total {
            return None;
        }
        for f in &self.frames[key_i + 1..=index] {
            match f {
                Frame::Diff {
                    t_ms: t,
                    cursor: c,
                    default_fg: dfg,
                    default_bg: dbg,
                    cursor_color: cc,
                    cursor_accent: ca,
                    mouse_cursor: mc,
                    pointer_shape: ps,
                    title: ttl,
                    changes,
                } => {
                    t_ms = *t;
                    cursor = *c;
                    default_fg = *dfg;
                    default_bg = *dbg;
                    cursor_color = *cc;
                    cursor_accent = *ca;
                    mouse_cursor = *mc;
                    pointer_shape = ps.unwrap_or(PointerShape::Default);
                    title = ttl.clone();
                    for ch in changes {
                        if let Some(slot) = cells.get_mut(ch.idx as usize) {
                            *slot = ch.cell.clone();
                        }
                    }
                }
                Frame::Key { .. } => unreachable!(),
            }
        }
        Some(RawFrame {
            t_ms,
            cols: self.cols,
            rows: self.rows,
            cells,
            cursor,
            default_fg,
            default_bg,
            cursor_color,
            cursor_accent,
            mouse_cursor,
            pointer_shape,
            title,
        })
    }
}
