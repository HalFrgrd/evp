//! Shared raw-frame consumer types and constants.

use crate::FrameStyle;

// Raw-frame consumers can briefly lag behind capture on busy systems; this
// queue absorbs bursts so the upstream pipeline usually stays lock-free.
pub const RAW_FRAME_CONSUMER_CHANNEL_CAPACITY: usize = 4096;

/// Canonical description of the terminal viewport used by every renderer and
/// recording consumer. Centralises all size / grid / style fields so they do
/// not have to be duplicated across `GifStreamConfig`, `SvgStreamConfig`,
/// `JsonStreamConfig`, `RendererConfig`, etc.
#[derive(Debug, Clone, Copy)]
pub struct ViewportConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub frame_style: FrameStyle,
}

#[derive(Clone)]
pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    /// Multiplier applied to `font_size` to produce the per-cell pixel
    /// height. Mirrors VHS / xterm.js `lineHeight`. A value of `1.0` makes
    /// cells exactly `font_size` pixels tall, matching xterm.js's CSS
    /// semantics.
    pub line_height: f32,
    pub frame_style: FrameStyle,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            font_path: None,
            font_size: 22.0,
            line_height: 1.0,
            frame_style: FrameStyle::default(),
        }
    }
}
