//! Shared raw-frame consumer types and constants.

use crate::FrameStyle;

// Raw-frame consumers can briefly lag behind capture on busy systems; this
// queue absorbs bursts so the upstream pipeline usually stays lock-free.
pub const RAW_FRAME_CONSUMER_CHANNEL_CAPACITY: usize = 4096;

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
