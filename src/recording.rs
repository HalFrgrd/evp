//! Recording artifact: a sequence of terminal frames captured at a fixed
//! framerate.
//!
//! The runner produces dense [`RawFrame`]s at the target framerate. The
//! encoder thread folds them into a [`Recording`] which keeps the first
//! frame in full and subsequent frames as **cell diffs** against the prior
//! one. This dramatically shrinks JSON serialisation while still being a
//! lossless representation of the captured terminal state.

use serde::{Deserialize, Serialize};

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

/// A complete terminal grid plus cursor state, captured at a single point
/// in time. Produced by the runner thread and shipped over a channel to
/// the encoder thread.
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
        cells: Vec<CellSnap>,
    },
    Diff {
        t_ms: u32,
        cursor: Option<(u16, u16)>,
        default_fg: [u8; 3],
        default_bg: [u8; 3],
        changes: Vec<CellChange>,
    },
}

impl Frame {
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
}

/// The full recording artifact (de)serialisable to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub padding_px: u32,
    pub frames: Vec<Frame>,
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
        match &self.frames[key_i] {
            Frame::Key {
                t_ms: t,
                cursor: c,
                default_fg: dfg,
                default_bg: dbg,
                cells: cs,
            } => {
                cells = cs.clone();
                t_ms = *t;
                cursor = *c;
                default_fg = *dfg;
                default_bg = *dbg;
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
                    changes,
                } => {
                    t_ms = *t;
                    cursor = *c;
                    default_fg = *dfg;
                    default_bg = *dbg;
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
        })
    }
}
