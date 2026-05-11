//! Shared raw-frame consumer types and constants.

use crate::FrameStyle;

// Raw-frame consumers can briefly lag behind capture on busy systems; this
// queue absorbs bursts so the upstream pipeline usually stays lock-free.
pub const RAW_FRAME_CONSUMER_CHANNEL_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub frame_style: FrameStyle,
}
