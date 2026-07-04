//! Shared raw-frame consumer types and constants.

use crate::{FrameStyle, Theme};

// Raw-frame consumers can briefly lag behind capture on busy systems; this
// queue absorbs bursts so the upstream pipeline usually stays lock-free.
pub const RAW_FRAME_CONSUMER_CHANNEL_CAPACITY: usize = 4096;

/// Canonical description of the terminal viewport used by every renderer and
/// recording consumer. Centralises all size / grid / style fields so they do
/// not have to be duplicated across `GifStreamConfig`, `SvgStreamConfig`,
/// `JsonStreamConfig`, `RendererConfig`, etc.
///
/// Construct via [`ViewportConfig::new`]; the derived layout fields
/// (`canvas_w`, `canvas_h`, `frame_x`, etc.) are computed automatically.
#[derive(Debug, Clone, Copy)]
pub struct ViewportConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub frame_style: FrameStyle,
    // Derived pixel-level layout geometry (computed by `new`).
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub frame_x: u32,
    pub frame_y: u32,
    pub frame_w: u32,
    pub frame_h: u32,
    pub bar_h: u32,
    pub content_x: u32,
    pub content_y: u32,
    /// The CSS em-square size used when the font metrics below were measured.
    /// Zero when font metrics are unavailable.
    pub font_size_px: f32,
    /// Raw font bbox height (`ascent − descent`) at `font_size_px`, without
    /// the `line_height` multiplier.  Zero when unavailable.
    pub char_height_px: u32,
    /// Raw scaled font ascent at `font_size_px` (before vertical centering).
    /// Zero when unavailable.
    pub ascent_px: u32,
    /// Additive pixels added to each cell's width.
    pub letter_spacing: f32,
}

/// Returns `true` if the character is a box drawing character.
/// This includes the Box Drawing block (U+2500..=U+257F), Block Elements (U+2580..=U+259F),
/// Symbols for Legacy Computing (U+1FB00..=U+1FBFF), and Symbols for Legacy Computing Supplement (U+1CC00..=U+1CEBF).
pub fn is_box_drawing(c: char) -> bool {
    let cp = c as u32;
    (0x2500..=0x259F).contains(&cp)
        || (0x1FB00..=0x1FBFF).contains(&cp)
        || (0x1CC00..=0x1CEBF).contains(&cp)
}

impl ViewportConfig {
    pub fn new(
        cols: u16,
        rows: u16,
        framerate: u32,
        cell_width_px: u32,
        cell_height_px: u32,
        frame_style: FrameStyle,
        font_size_px: f32,
        char_height_px: u32,
        ascent_px: u32,
        letter_spacing: f32,
    ) -> Self {
        let cell_w = cell_width_px.max(1);
        let cell_h = cell_height_px.max(1);
        let bar_h = if frame_style.window_bar.enabled() {
            frame_style.window_bar_size_px
        } else {
            0
        };
        let grid_frame_w = cols as u32 * cell_w + frame_style.padding_px * 2;
        let grid_frame_h = rows as u32 * cell_h + frame_style.padding_px * 2 + bar_h;
        let canvas_w = frame_style
            .canvas_width_px
            .unwrap_or(grid_frame_w + frame_style.margin_px * 2)
            .max(1);
        let canvas_h = frame_style
            .canvas_height_px
            .unwrap_or(grid_frame_h + frame_style.margin_px * 2)
            .max(1);
        let frame_w = canvas_w.saturating_sub(frame_style.margin_px * 2).max(1);
        let frame_h = canvas_h.saturating_sub(frame_style.margin_px * 2).max(1);
        let inner_w = frame_w.saturating_sub(frame_style.padding_px * 2);
        let inner_h = frame_h.saturating_sub(frame_style.padding_px * 2 + bar_h);
        let grid_w = (cols as u32 * cell_w).min(inner_w);
        let grid_h = (rows as u32 * cell_h).min(inner_h);
        let extra_x = inner_w.saturating_sub(grid_w) / 2;
        let extra_y = inner_h.saturating_sub(grid_h) / 2;
        Self {
            cols,
            rows,
            framerate,
            cell_width_px,
            cell_height_px,
            frame_style,
            canvas_w,
            canvas_h,
            frame_x: frame_style.margin_px,
            frame_y: frame_style.margin_px,
            frame_w,
            frame_h,
            bar_h,
            content_x: frame_style.margin_px + frame_style.padding_px + extra_x,
            content_y: frame_style.margin_px + bar_h + frame_style.padding_px + extra_y,
            font_size_px,
            char_height_px,
            ascent_px,
            letter_spacing,
        }
    }
}

#[derive(Clone)]
pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    /// Multiplier applied to the font's bounding-box height to produce the
    /// per-cell pixel height. Mirrors VHS / xterm.js `lineHeight`. A value
    /// of `1.0` makes cells exactly one bounding-box height tall.
    /// // lineHeight — MULTIPLIER on char height:
    /// this.dimensions.device.cell.height =
    ///     Math.floor(this.dimensions.device.char.height * this._optionsService.rawOptions.lineHeight);
    pub line_height: f32,
    /// Extra pixels added to each cell's width. Mirrors VHS / xterm.js
    /// `letterSpacing`. The default `1.0` adds one pixel of trailing space
    /// per column, matching the VHS default.
    /// // letterSpacing — ADDITIVE pixels added to char advance:
    /// this.dimensions.device.cell.width =
    ///     this.dimensions.device.char.width + Math.round(this._optionsService.rawOptions.letterSpacing);
    pub letter_spacing: f32,
    pub frame_style: FrameStyle,
    pub no_system_fonts: bool,
    pub theme: Theme,
    pub window_bar_title: Option<String>,
    pub window_bar_font_family: Option<String>,
    pub window_bar_font_size: Option<f32>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            font_path: None,
            font_size: 22.0,
            line_height: 1.0,
            letter_spacing: 1.0,
            frame_style: FrameStyle::default(),
            no_system_fonts: false,
            theme: Theme::vhs_default(),
            window_bar_title: None,
            window_bar_font_family: None,
            window_bar_font_size: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_box_drawing() {
        assert!(is_box_drawing('╭')); // Box Drawing
        assert!(is_box_drawing('█')); // Block Elements
        assert!(is_box_drawing('\u{1FB00}')); // Legacy Computing
        assert!(is_box_drawing('\u{1CC00}')); // Legacy Computing Supplement

        assert!(!is_box_drawing('A'));
        assert!(!is_box_drawing(' '));
    }
}
