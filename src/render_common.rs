//! Shared renderer types and constants.

use crate::FrameStyle;

// Rendering can briefly lag behind capture on busy systems; this queue absorbs
// bursts so the upstream pipeline usually stays lock-free.
pub const RENDER_STREAM_CHANNEL_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub frame_style: FrameStyle,
}
