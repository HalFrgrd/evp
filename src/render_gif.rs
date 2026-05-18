//! Render a [`Recording`] to an animated GIF using gifski with streaming.
//!
//! We rasterise each frame as an RGBA buffer using `ab_glyph`, then stream
//! frames directly to gifski's collector. This allows encoding to happen
//! concurrently with recording, reducing peak memory and latency.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use ab_glyph::{Font, FontArc, Glyph, GlyphId, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};
use gifski::{Settings, progress};
use tracing::{debug, warn};
use woff2_patched::convert_woff2_to_ttf;

use crate::{
    FrameStyle,
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, RenderOptions, ViewportConfig},
    style::window_bar_dot_metrics,
};

const EMBEDDED_JETBRAINS_NERD_MONO_REGULAR_WOFF2: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/JetBrainsMonoNerdFontMono-Regular.woff2"
));
const EMBEDDED_JETBRAINS_NERD_MONO_BOLD_WOFF2: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/JetBrainsMonoNerdFontMono-Bold.woff2"
));
const EMBEDDED_JETBRAINS_NERD_MONO_ITALIC_WOFF2: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/JetBrainsMonoNerdFontMono-Italic.woff2"
));
const EMBEDDED_JETBRAINS_NERD_MONO_BOLD_ITALIC_WOFF2: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/JetBrainsMonoNerdFontMono-BoldItalic.woff2"
));
const EMBEDDED_UNIFONT_UPPER_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/unifont_upper-17.0.04.woff2"));
const EMBEDDED_UNIFONT_CSUR_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/unifont_csur-17.0.04.woff2"));
const EMBEDDED_NOTO_SANS_MONO_REGULAR_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/NotoSansMono-Regular.woff2"));
const EMBEDDED_NOTO_SANS_SYMBOLS2_REGULAR_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/NotoSansSymbols2-Regular.woff2"));
const EMBEDDED_NOTO_SANS_MONO_CJK_JP_SUBSET_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/NotoSansMonoCJKjp-Subset.woff2"));

/// A collection of font faces organised by text style.
///
/// Each style variant holds an ordered list of indices into `fonts`.  When
/// rendering a character the list is walked in order and the first face that
/// contains a glyph for that character wins.  The primary face is at index 0
/// of each per-style list; everything after it is a fallback.  There is no
/// distinction between "primary" and "fallback" — they are all just entries
/// in a `Vec` to be tried in sequence.
#[derive(Debug)]
struct FontSet {
    /// All loaded font faces, indexed.
    fonts: Vec<FontArc>,
    /// Font indices (into `fonts`) to try for regular text, in priority order.
    regular: Vec<usize>,
    /// Font indices to try for bold text.
    bold: Vec<usize>,
    /// Font indices to try for italic text.
    italic: Vec<usize>,
    /// Font indices to try for bold-italic text.
    bold_italic: Vec<usize>,
}

impl FontSet {
    /// Returns the ordered font-index slice appropriate for `flags`.
    ///
    /// If the style-specific list is empty (e.g. no bold face was loaded) the
    /// regular list is returned as a fallback.
    fn indices_for_flags(&self, flags: u8) -> &[usize] {
        let want_bold = flags & style_flags::BOLD != 0;
        let want_italic = flags & style_flags::ITALIC != 0;
        let list = match (want_bold, want_italic) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        };
        if list.is_empty() { &self.regular } else { list }
    }

    /// Select the best `(font_index, &FontArc)` for `ch` given cell `flags`.
    ///
    /// Tries each face in the appropriate style list in order; returns the
    /// first that has a glyph for `ch`.  Falls back to the first face in the
    /// list if none do.
    fn select_for_char(&self, flags: u8, ch: char) -> (usize, &FontArc) {
        let indices = self.indices_for_flags(flags);
        for &idx in indices {
            if has_glyph(&self.fonts[idx], ch) {
                return (idx, &self.fonts[idx]);
            }
        }
        let idx = indices[0];
        (idx, &self.fonts[idx])
    }
}

/// Cache key identifying a rasterised glyph outline.
#[derive(Hash, Eq, PartialEq)]
struct GlyphCacheKey {
    /// Index into [`FontSet::fonts`].
    font_idx: u16,
    /// ab_glyph glyph identifier within the face.
    glyph_id: u16,
    /// Uniform px-scale as `f32` bits (we only use uniform scales).
    scale_bits: u32,
}

/// Colour-independent coverage mask for one rasterised glyph.
///
/// Storing coverage separately from colour lets the same cached bitmap be
/// blended with any foreground colour without re-rasterising.
struct GlyphBitmap {
    /// Horizontal pixel offset from the pen position to the bitmap's left
    /// edge (equal to `px_bounds().min.x` rounded to integer).
    offset_x: i32,
    /// Vertical pixel offset from the baseline to the bitmap's top edge
    /// (equal to `px_bounds().min.y` rounded to integer).
    offset_y: i32,
    width: u32,
    height: u32,
    /// Per-pixel coverage in row-major order [height × width].
    pixels: Vec<f32>,
}

/// Per-session glyph rasterisation cache.
///
/// Maps a `(font_idx, glyph_id, scale)` key to either `None` (the glyph has
/// no visible outline — e.g. space) or `Some(bitmap)` with coverage data that
/// can be blended with any foreground colour.
type GlyphCache = HashMap<GlyphCacheKey, Option<GlyphBitmap>>;

#[derive(Clone, Copy)]
struct LayoutMetrics {
    canvas_w: u32,
    canvas_h: u32,
    frame_x: u32,
    frame_y: u32,
    frame_w: u32,
    frame_h: u32,
    bar_h: u32,
    content_x: u32,
    content_y: u32,
}

pub struct GifStreamHandle {
    pub tx: Sender<RawFrame>,
    join: JoinHandle<Result<()>>,
}

impl GifStreamHandle {
    pub fn join(self) -> Result<()> {
        drop(self.tx);
        self.join.join().expect("gif stream worker panicked")
    }
}

/// Compute cell metrics that mirror VHS / xterm.js CSS semantics.
///
/// xterm.js's `fontSize: N` sets the CSS em-square (i.e. `units_per_em` worth
/// of design units) to `N` pixels. The horizontal advance of a monospace
/// glyph is then `advance_units * N / upem` — for JetBrains Mono at `N=22`
/// this gives `600 * 22 / 1000 = 13.2 px`.
///
/// `ab_glyph::PxScale::from(N)` instead scales so that the font's
/// `ascent - descent` (NOT upem) equals `N`. For JetBrains Mono that means
/// the same `N=22` produces only `600 * 22 / 1320 ≈ 10 px` of advance —
/// noticeably narrower than VHS, which is what made the evp demos look
/// "zoomed out" compared to the upstream VHS recordings.
///
/// This helper picks a `PxScale` such that one em-square equals `font_size`
/// pixels, and returns:
///   * the `PxScale` to feed to ab_glyph for rasterisation,
///   * `cell_w`: the integer per-cell horizontal advance,
///   * `cell_h`: `round(font_size * line_height)` to match xterm.js's
///     `lineHeight`-driven cell height,
///   * `baseline`: the baseline offset from the top of the cell, computed
///     from the font's ascent in CSS pixels.
fn css_cell_metrics(font: &FontArc, font_size: f32, line_height: f32) -> (PxScale, u32, u32, u32) {
    // Fall back to the font's intrinsic height if upem is missing.
    let upem = font
        .units_per_em()
        .unwrap_or_else(|| font.height_unscaled());
    let height_units = font.height_unscaled().max(1.0);
    // ab_glyph: scaled_metric = unscaled_metric * scale.x / height_units. To
    // make an em-square render at exactly `font_size` pixels we need
    // `scale.x = font_size * height_units / upem`.
    let px_scale = font_size * height_units / upem;
    let scale = PxScale::from(px_scale);
    let scaled = font.as_scaled(scale);
    let cell_w = scaled.h_advance(font.glyph_id('M')).ceil().max(1.0) as u32;
    let cell_h = (font_size * line_height).round().max(1.0) as u32;
    // Place the baseline at the font's ascent expressed in CSS pixels. For
    // most monospace fonts this fits inside the cell; if the ascent is
    // slightly larger than `cell_h` the glyph clips at the top exactly the
    // way xterm.js / browsers handle it.
    let baseline = (font_size * font.ascent_unscaled() / upem).round().max(0.0) as u32;
    (scale, cell_w, cell_h, baseline)
}

/// Load the font family for `font_path` (or the embedded default) and return
/// `(cell_w_px, cell_h_px)` using the same CSS em-square semantics as
/// [`css_cell_metrics`].
///
/// Called by the runner to derive the terminal grid size before any renderer
/// is spawned, so cols/rows are computed from the actual font metrics rather
/// than a geometric approximation.
pub fn measure_cell_px(font_path: Option<&str>, font_size: f32, line_height: f32) -> (u32, u32) {
    let font_set = match load_font_family(font_path) {
        Ok(loaded) => loaded.font_set,
        Err(_) => {
            // Fallback: 0.6 em approximation — only reached if the embedded
            // fonts are somehow corrupt.
            let w = (font_size * 0.6).round().max(1.0) as u32;
            let h = (font_size * line_height).round().max(1.0) as u32;
            return (w, h);
        }
    };
    let primary = &font_set.fonts[font_set.regular[0]];
    let (_scale, cell_w, cell_h, _baseline) = css_cell_metrics(primary, font_size, line_height);
    // Scale up the measured cell dimensions so the runner computes a col/row
    // count that matches what a browser's font engine produces. VHS's FitAddon
    // runs in a real Chromium context and consistently renders cells roughly
    // 1.Y× wider/taller than the raw advance from the font file.
    const CELL_SCALE: f32 = 1.03;
    let w = ((cell_w as f32) * CELL_SCALE).round().max(1.0) as u32;
    let h = ((cell_h as f32) * CELL_SCALE).round().max(1.0) as u32;
    (w, h)
}

pub fn spawn_gif_stream(
    cfg: ViewportConfig,
    opts: RenderOptions,
    output: PathBuf,
) -> Result<GifStreamHandle> {
    let loaded = load_font_family(opts.font_path.as_deref())?;
    debug!(font = %loaded.description, "using font for gif streaming");

    let font_set = loaded.font_set;
    let primary = &font_set.fonts[font_set.regular[0]];
    let (scale, cell_w, cell_h, baseline) =
        css_cell_metrics(primary, opts.font_size, opts.line_height);
    let layout = layout_metrics(cfg.cols, cfg.rows, cell_w, cell_h, cfg.frame_style);

    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RAW_FRAME_CONSUMER_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-gif-stream".into())
        .spawn(move || {
            run_gif_stream_worker(
                rx,
                output,
                font_set,
                scale,
                cell_w,
                cell_h,
                baseline,
                cfg.frame_style,
                layout,
            )
        })
        .expect("failed to spawn gif stream worker");

    Ok(GifStreamHandle { tx, join })
}

pub fn render_gif(rec: &Recording, opts: &RenderOptions, out: &Path) -> Result<()> {
    let stream = spawn_gif_stream(
        ViewportConfig {
            cols: rec.cols,
            rows: rec.rows,
            framerate: rec.framerate,
            cell_width_px: rec.cell_width_px,
            cell_height_px: rec.cell_height_px,
            frame_style: rec.frame_style,
        },
        opts.clone(),
        out.to_path_buf(),
    )?;

    for i in 0..rec.frames.len() {
        let frame = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        if stream.tx.send(frame).is_err() {
            break;
        }
    }

    stream.join()
}

pub fn render_png_frame(frame: &RawFrame, opts: &RenderOptions, out: &Path) -> Result<()> {
    let loaded = load_font_family(opts.font_path.as_deref())?;
    let font_set = loaded.font_set;
    let primary = &font_set.fonts[font_set.regular[0]];
    let (scale, cell_w, cell_h, baseline) =
        css_cell_metrics(primary, opts.font_size, opts.line_height);
    let mut glyph_cache = GlyphCache::new();
    let buf = rasterize_raw_frame(
        frame,
        &font_set,
        scale,
        cell_w,
        cell_h,
        baseline,
        opts.frame_style,
        &mut glyph_cache,
    );
    let layout = layout_metrics(frame.cols, frame.rows, cell_w, cell_h, opts.frame_style);
    lodepng::encode24_file(
        out,
        &buf,
        layout.canvas_w as usize,
        layout.canvas_h as usize,
    )
    .with_context(|| format!("encoding {}", out.display()))
}

/// Convert RGB (3-byte) to RGBA (4-byte) with full alpha.
fn rgb_to_rgba(rgb: &[u8]) -> Vec<rgb::RGBA<u8>> {
    let mut rgba = Vec::with_capacity(rgb.len() / 3);
    for chunk in rgb.chunks(3) {
        rgba.push(rgb::RGBA {
            r: chunk[0],
            g: chunk[1],
            b: chunk[2],
            a: 255, // fully opaque
        });
    }
    rgba
}

#[allow(clippy::too_many_arguments)]
fn run_gif_stream_worker(
    rx: Receiver<RawFrame>,
    out: PathBuf,
    font_set: FontSet,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    frame_style: FrameStyle,
    layout: LayoutMetrics,
) -> Result<()> {
    let (collector, writer) = gifski::new(Settings {
        width: Some(layout.canvas_w),
        height: Some(layout.canvas_h),
        quality: 100,
        fast: false,
        repeat: gifski::Repeat::Infinite,
    })
    .context("initialize gifski encoder")?;

    let mut glyph_cache = GlyphCache::new();
    let mut last_seen_t_ms = 0u32;
    let mut last_emitted_t_ms = 0u32;
    let mut prev_buf: Option<Vec<u8>> = None;
    let mut frame_index = 0usize;

    // The gifski writer must run concurrently with frame ingestion: the
    // collector has a bounded internal queue, so `add_frame_rgba` blocks
    // when the writer falls behind. Spawn the writer on its own thread and
    // join it after we drop the collector (which signals EOF to gifski).
    let out_path = out.clone();
    let writer_handle = thread::Builder::new()
        .name("evp-gif-writer".into())
        .spawn(move || {
            let file = std::fs::File::create(&out_path)
                .with_context(|| format!("create {}", out_path.display()))?;
            let mut p = progress::NoProgress {};
            writer
                .write(file, &mut p)
                .map_err(|e| anyhow!("gifski write error: {e}"))
        })
        .expect("failed to spawn gif writer thread");

    while let Ok(frame) = rx.recv() {
        let buf = rasterize_raw_frame(
            &frame,
            &font_set,
            scale,
            cell_w,
            cell_h,
            baseline,
            frame_style,
            &mut glyph_cache,
        );

        last_seen_t_ms = frame.t_ms;

        if prev_buf.is_none() {
            // gifski expects absolute presentation timestamps and the first
            // frame at t=0. Emit the very first captured frame unconditionally
            // so leading sleeps are represented correctly.
            let rgba = rgb_to_rgba(&buf);
            let frame_img =
                imgref::ImgVec::new(rgba, layout.canvas_w as usize, layout.canvas_h as usize);
            collector
                .add_frame_rgba(frame_index, frame_img, 0.0)
                .context("add first frame to gifski")?;
            frame_index += 1;
            last_emitted_t_ms = frame.t_ms;
            prev_buf = Some(buf);
            continue;
        }

        if prev_buf.as_ref() == Some(&buf) {
            continue;
        }

        let rgba = rgb_to_rgba(&buf);
        let frame_img =
            imgref::ImgVec::new(rgba, layout.canvas_w as usize, layout.canvas_h as usize);
        collector
            .add_frame_rgba(frame_index, frame_img, frame.t_ms as f64 / 1000.0)
            .context("add frame to gifski")?;

        frame_index += 1;
        last_emitted_t_ms = frame.t_ms;
        prev_buf = Some(buf);
    }

    // If capture ended on unchanged frames (common for trailing Sleep),
    // flush the trailing delay by duplicating the last emitted frame at
    // the final absolute timestamp.
    if last_seen_t_ms > last_emitted_t_ms
        && let Some(buf) = prev_buf.as_ref()
    {
        let rgba = rgb_to_rgba(buf);
        let frame_img =
            imgref::ImgVec::new(rgba, layout.canvas_w as usize, layout.canvas_h as usize);
        collector
            .add_frame_rgba(frame_index, frame_img, last_seen_t_ms as f64 / 1000.0)
            .context("add trailing delay frame to gifski")?;
    }

    drop(collector);
    writer_handle
        .join()
        .map_err(|_| anyhow!("gif writer thread panicked"))?
        .context("write gif")?;
    Ok(())
}

fn rasterize_raw_frame(
    frame: &RawFrame,
    font_set: &FontSet,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    frame_style: FrameStyle,
    glyph_cache: &mut GlyphCache,
) -> Vec<u8> {
    let layout = layout_metrics(frame.cols, frame.rows, cell_w, cell_h, frame_style);
    let mut buf = vec![0u8; (layout.canvas_w * layout.canvas_h * 3) as usize];

    fill_rect(
        &mut buf,
        layout.canvas_w,
        0,
        0,
        layout.canvas_w,
        layout.canvas_h,
        frame_style.margin_fill,
    );
    fill_rect(
        &mut buf,
        layout.canvas_w,
        layout.frame_x,
        layout.frame_y,
        layout.frame_w,
        layout.frame_h,
        frame.default_bg,
    );
    if frame_style.window_bar.enabled() {
        draw_window_bar(&mut buf, layout.canvas_w, layout, frame_style.window_bar);
    }

    for row in 0..frame.rows {
        for col in 0..frame.cols {
            let idx = row as usize * frame.cols as usize + col as usize;
            let cell = &frame.cells[idx];
            let x = layout.content_x + col as u32 * cell_w;
            let y = layout.content_y + row as u32 * cell_h;

            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & style_flags::INVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }

            if bg != frame.default_bg || cell.flags & style_flags::INVERSE != 0 {
                fill_rect(&mut buf, layout.canvas_w, x, y, cell_w, cell_h, bg);
            }

            if cell.text.is_empty() {
                continue;
            }

            let mut pen_x = x as f32;
            let pen_y_baseline = y as i32 + baseline as i32;
            for ch in cell.text.chars() {
                let (font_idx, font) = font_set.select_for_char(cell.flags, ch);
                let glyph_id: GlyphId = font.glyph_id(ch);

                // Populate the cache on first encounter of this
                // (font, glyph, scale) combination.
                //
                // font_idx is the index into FontSet::fonts; the total
                // number of faces is small (< 10 for the default set) so
                // u16 is always sufficient.
                debug_assert!(
                    font_idx <= u16::MAX as usize,
                    "font_idx {font_idx} exceeds u16 range"
                );
                let cache_key = GlyphCacheKey {
                    font_idx: font_idx as u16,
                    glyph_id: glyph_id.0,
                    scale_bits: scale.x.to_bits(),
                };
                let bitmap = glyph_cache.entry(cache_key).or_insert_with(|| {
                    let glyph: Glyph = glyph_id.with_scale(scale);
                    font.outline_glyph(glyph).map(|outline| {
                        let bounds = outline.px_bounds();
                        let w = (bounds.max.x - bounds.min.x).ceil() as u32;
                        let h = (bounds.max.y - bounds.min.y).ceil() as u32;
                        let mut pixels = vec![0.0f32; (w * h) as usize];
                        outline.draw(|gx, gy, coverage| {
                            let i = (gy * w + gx) as usize;
                            if i < pixels.len() {
                                pixels[i] = coverage;
                            }
                        });
                        GlyphBitmap {
                            offset_x: bounds.min.x as i32,
                            offset_y: bounds.min.y as i32,
                            width: w,
                            height: h,
                            pixels,
                        }
                    })
                });

                if let Some(bm) = bitmap.as_ref() {
                    for gy in 0..bm.height {
                        for gx in 0..bm.width {
                            let coverage = bm.pixels[(gy * bm.width + gx) as usize];
                            if coverage <= 0.0 {
                                continue;
                            }
                            let px = pen_x as i32 + bm.offset_x + gx as i32;
                            let py = pen_y_baseline + bm.offset_y + gy as i32;
                            if px < 0 || py < 0 {
                                continue;
                            }
                            let (px, py) = (px as u32, py as u32);
                            if px >= layout.canvas_w || py >= layout.canvas_h {
                                continue;
                            }
                            blend_pixel(&mut buf, layout.canvas_w, px, py, fg, coverage);
                        }
                    }
                }

                let scaled = font.as_scaled(scale);
                pen_x += scaled.h_advance(glyph_id);
            }

            if cell.flags & style_flags::UNDERLINE != 0 {
                let uy = y + cell_h.saturating_sub(2);
                fill_rect(&mut buf, layout.canvas_w, x, uy, cell_w, 1, fg);
            }
        }
    }

    if let Some((cx, cy)) = frame.cursor {
        let x = layout.content_x + cx as u32 * cell_w;
        let y = layout.content_y + cy as u32 * cell_h;
        invert_rect(&mut buf, layout.canvas_w, x, y, cell_w, cell_h);
    }

    if frame_style.border_radius_px > 0 {
        mask_outside_rounded_rect(
            &mut buf,
            layout.canvas_w,
            layout,
            frame_style.border_radius_px,
            frame_style.margin_fill,
        );
    }

    buf
}

fn layout_metrics(
    cols: u16,
    rows: u16,
    cell_w: u32,
    cell_h: u32,
    frame_style: FrameStyle,
) -> LayoutMetrics {
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

    // Centre the cell grid inside the padded content area. If the user-supplied
    // canvas size leaves a few stray pixels after the grid (because
    // `canvas_w - 2*(padding+margin)` is not an exact multiple of cell_w),
    // split them evenly between the left/right (and top/bottom) padding so the
    // visible margins are symmetric — matching VHS, which centres the
    // recorded terminal inside its frame with ffmpeg `pad=...(ow-iw)/2`.
    // let inner_w = frame_w.saturating_sub(frame_style.padding_px * 2);
    // let inner_h = frame_h.saturating_sub(frame_style.padding_px * 2 + bar_h);
    // let grid_w = (cols as u32 * cell_w).min(inner_w);
    // let grid_h = (rows as u32 * cell_h).min(inner_h);
    // let extra_x = inner_w.saturating_sub(grid_w) / 2;
    // let extra_y = inner_h.saturating_sub(grid_h) / 2;

    LayoutMetrics {
        canvas_w,
        canvas_h,
        frame_x: frame_style.margin_px,
        frame_y: frame_style.margin_px,
        frame_w,
        frame_h,
        bar_h,
        content_x: frame_style.margin_px + frame_style.padding_px,
        content_y: frame_style.margin_px + bar_h + frame_style.padding_px,
    }
}

fn draw_window_bar(buf: &mut [u8], w: u32, layout: LayoutMetrics, style: crate::WindowBarStyle) {
    let bar_h = layout.bar_h;
    let (radius, gap) = window_bar_dot_metrics(bar_h);
    let dots_w = radius * 2 * 3 + gap * 2;
    let start_x = if style.align_right() {
        layout.frame_x + layout.frame_w.saturating_sub(dots_w + gap)
    } else {
        layout.frame_x + gap
    };
    let cy = layout.frame_y + bar_h / 2;
    for (idx, color) in [[255, 95, 86], [255, 189, 46], [39, 201, 63]]
        .iter()
        .enumerate()
    {
        let cx = start_x + idx as u32 * (radius * 2 + gap) + radius;
        if style.outlined() {
            draw_circle_outline(buf, w, cx, cy, radius, *color, 2);
        } else {
            fill_circle(buf, w, cx, cy, radius, *color);
        }
    }
}

fn fill_circle(buf: &mut [u8], w: u32, cx: u32, cy: u32, radius: u32, color: [u8; 3]) {
    let r2 = (radius * radius) as i64;
    for y in cy.saturating_sub(radius)..=cy + radius {
        for x in cx.saturating_sub(radius)..=cx + radius {
            let dx = x as i64 - cx as i64;
            let dy = y as i64 - cy as i64;
            if dx * dx + dy * dy <= r2 {
                let i = ((y * w + x) * 3) as usize;
                if i + 2 < buf.len() {
                    buf[i] = color[0];
                    buf[i + 1] = color[1];
                    buf[i + 2] = color[2];
                }
            }
        }
    }
}

fn draw_circle_outline(
    buf: &mut [u8],
    w: u32,
    cx: u32,
    cy: u32,
    radius: u32,
    color: [u8; 3],
    thickness: u32,
) {
    let outer = (radius * radius) as i64;
    let inner_radius = radius.saturating_sub(thickness);
    let inner = (inner_radius * inner_radius) as i64;
    for y in cy.saturating_sub(radius)..=cy + radius {
        for x in cx.saturating_sub(radius)..=cx + radius {
            let dx = x as i64 - cx as i64;
            let dy = y as i64 - cy as i64;
            let d2 = dx * dx + dy * dy;
            if d2 <= outer && d2 >= inner {
                let i = ((y * w + x) * 3) as usize;
                if i + 2 < buf.len() {
                    buf[i] = color[0];
                    buf[i + 1] = color[1];
                    buf[i + 2] = color[2];
                }
            }
        }
    }
}

fn mask_outside_rounded_rect(
    buf: &mut [u8],
    w: u32,
    layout: LayoutMetrics,
    radius: u32,
    fill: [u8; 3],
) {
    let radius = radius.min(layout.frame_w / 2).min(layout.frame_h / 2) as i64;
    for y in layout.frame_y..layout.frame_y + layout.frame_h {
        for x in layout.frame_x..layout.frame_x + layout.frame_w {
            if !inside_rounded_rect(x, y, layout, radius) {
                let i = ((y * w + x) * 3) as usize;
                if i + 2 < buf.len() {
                    buf[i] = fill[0];
                    buf[i + 1] = fill[1];
                    buf[i + 2] = fill[2];
                }
            }
        }
    }
}

fn inside_rounded_rect(x: u32, y: u32, layout: LayoutMetrics, radius: i64) -> bool {
    if radius == 0 {
        return true;
    }
    let x = x as i64;
    let y = y as i64;
    let left = layout.frame_x as i64;
    let top = layout.frame_y as i64;
    let right = (layout.frame_x + layout.frame_w - 1) as i64;
    let bottom = (layout.frame_y + layout.frame_h - 1) as i64;
    if (x >= left + radius && x <= right - radius) || (y >= top + radius && y <= bottom - radius) {
        return true;
    }
    let (cx, cy) = if x < left + radius && y < top + radius {
        (left + radius, top + radius)
    } else if x > right - radius && y < top + radius {
        (right - radius, top + radius)
    } else if x < left + radius && y > bottom - radius {
        (left + radius, bottom - radius)
    } else {
        (right - radius, bottom - radius)
    };
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= radius * radius
}

fn fill_rect(buf: &mut [u8], w: u32, x: u32, y: u32, rw: u32, rh: u32, color: [u8; 3]) {
    for yy in y..(y + rh) {
        for xx in x..(x + rw) {
            let i = ((yy * w + xx) * 3) as usize;
            if i + 2 < buf.len() {
                buf[i] = color[0];
                buf[i + 1] = color[1];
                buf[i + 2] = color[2];
            }
        }
    }
}

fn invert_rect(buf: &mut [u8], w: u32, x: u32, y: u32, rw: u32, rh: u32) {
    for yy in y..(y + rh) {
        for xx in x..(x + rw) {
            let i = ((yy * w + xx) * 3) as usize;
            if i + 2 < buf.len() {
                buf[i] = 255 - buf[i];
                buf[i + 1] = 255 - buf[i + 1];
                buf[i + 2] = 255 - buf[i + 2];
            }
        }
    }
}

fn blend_pixel(buf: &mut [u8], w: u32, x: u32, y: u32, color: [u8; 3], coverage: f32) {
    let i = ((y * w + x) * 3) as usize;
    if i + 2 >= buf.len() {
        return;
    }
    let a = coverage.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    for k in 0..3 {
        let bg = buf[i + k] as f32;
        let fg = color[k] as f32;
        buf[i + k] = (fg * a + bg * inv).round().clamp(0.0, 255.0) as u8;
    }
}

/// Load the requested font set. If `path` is provided it is used as the sole
/// face for all text styles. Otherwise the embedded JetBrains Mono Nerd Font
/// faces are loaded as primaries with embedded fallback faces appended.
#[derive(Debug)]
struct LoadedFontFamily {
    font_set: FontSet,
    description: String,
}

fn load_font_family(path: Option<&str>) -> Result<LoadedFontFamily> {
    if let Some(p) = path {
        let bytes = std::fs::read(p).with_context(|| format!("reading font {p}"))?;
        let face = FontArc::try_from_vec(bytes).context("invalid font file")?;
        // Use the same face for all styles; no fallbacks for custom fonts.
        let font_set = FontSet {
            fonts: vec![face],
            regular: vec![0],
            bold: vec![0],
            italic: vec![0],
            bold_italic: vec![0],
        };
        return Ok(LoadedFontFamily {
            font_set,
            description: format!("explicit path: {p}"),
        });
    }

    // Deterministic default: embedded JetBrains Mono Nerd Font (Mono variant)
    // compressed as WOFF2 at build time and decompressed at runtime.
    // License text is shipped in `licenses/JETBRAINSMONO-OFL-1.1.txt`.
    let jb_regular = decode_embedded_face(
        "JetBrainsMonoNerdFontMono-Regular.woff2",
        EMBEDDED_JETBRAINS_NERD_MONO_REGULAR_WOFF2,
    )?;
    let jb_bold = try_embedded_face(
        "JetBrainsMonoNerdFontMono-Bold.woff2",
        EMBEDDED_JETBRAINS_NERD_MONO_BOLD_WOFF2,
    );
    let jb_italic = try_embedded_face(
        "JetBrainsMonoNerdFontMono-Italic.woff2",
        EMBEDDED_JETBRAINS_NERD_MONO_ITALIC_WOFF2,
    );
    let jb_bold_italic = try_embedded_face(
        "JetBrainsMonoNerdFontMono-BoldItalic.woff2",
        EMBEDDED_JETBRAINS_NERD_MONO_BOLD_ITALIC_WOFF2,
    );

    let (fallback_faces, fallback_names) = load_default_fallback_faces();

    // Build the flat font list and per-style index vectors.
    //
    // Layout:
    //   0  = JetBrains Regular
    //   1  = JetBrains Bold       (if loaded)
    //   2  = JetBrains Italic     (if loaded)
    //   3  = JetBrains BoldItalic (if loaded)
    //   4+ = fallback faces (NotoSansMono, NotoSansSymbols2, etc.)
    let mut fonts: Vec<FontArc> = Vec::new();
    fonts.push(jb_regular);
    let idx_bold = jb_bold.map(|f| {
        let i = fonts.len();
        fonts.push(f);
        i
    });
    let idx_italic = jb_italic.map(|f| {
        let i = fonts.len();
        fonts.push(f);
        i
    });
    let idx_bold_italic = jb_bold_italic.map(|f| {
        let i = fonts.len();
        fonts.push(f);
        i
    });

    let fallback_start = fonts.len();
    fonts.extend(fallback_faces);
    let fallback_indices: Vec<usize> = (fallback_start..fonts.len()).collect();

    // Each style list: the style-specific primary (if available) followed by
    // all fallback faces in order.  When a style face was not loaded its list
    // falls back to the regular list via `indices_for_flags`.
    let regular: Vec<usize> = std::iter::once(0)
        .chain(fallback_indices.iter().copied())
        .collect();
    let bold: Vec<usize> = idx_bold
        .into_iter()
        .chain(fallback_indices.iter().copied())
        .collect();
    let italic: Vec<usize> = idx_italic
        .into_iter()
        .chain(fallback_indices.iter().copied())
        .collect();
    let bold_italic: Vec<usize> = idx_bold_italic
        .into_iter()
        .chain(fallback_indices.iter().copied())
        .collect();

    let description = if fallback_names.is_empty() {
        "embedded default: JetBrainsMono Nerd Font Mono family".to_string()
    } else {
        format!(
            "embedded default: JetBrainsMono Nerd Font Mono family + fallbacks [{}]",
            fallback_names.join(" -> ")
        )
    };

    Ok(LoadedFontFamily {
        font_set: FontSet {
            fonts,
            regular,
            bold,
            italic,
            bold_italic,
        },
        description,
    })
}

fn load_default_fallback_faces() -> (Vec<FontArc>, Vec<String>) {
    let mut faces = Vec::new();
    let mut names = Vec::new();

    // 1) Embedded Noto Sans Mono (broad BMP + width-consistent text).
    match decode_embedded_face(
        "NotoSansMono-Regular.woff2",
        EMBEDDED_NOTO_SANS_MONO_REGULAR_WOFF2,
    ) {
        Ok(font) => {
            faces.push(font);
            names.push("NotoSansMono-Regular (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    // 2) Embedded Noto Sans Symbols 2 (symbols incl. Braille patterns).
    match decode_embedded_face(
        "NotoSansSymbols2-Regular.woff2",
        EMBEDDED_NOTO_SANS_SYMBOLS2_REGULAR_WOFF2,
    ) {
        Ok(font) => {
            faces.push(font);
            names.push("NotoSansSymbols2-Regular (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    // 3) Embedded Noto Sans Mono CJK JP subset (JP + half-width katakana).
    match decode_embedded_face(
        "NotoSansMonoCJKjp-Subset.woff2",
        EMBEDDED_NOTO_SANS_MONO_CJK_JP_SUBSET_WOFF2,
    ) {
        Ok(font) => {
            faces.push(font);
            names.push("NotoSansMonoCJKjp-Subset (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    // 4) Embedded unifont_upper (U+10000 and above coverage).
    match decode_embedded_face("unifont_upper-17.0.04.woff2", EMBEDDED_UNIFONT_UPPER_WOFF2) {
        Ok(font) => {
            faces.push(font);
            names.push("unifont_upper-17.0.04 (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    // 5) Embedded unifont_csur (CSUR/PUA coverage).
    match decode_embedded_face("unifont_csur-17.0.04.woff2", EMBEDDED_UNIFONT_CSUR_WOFF2) {
        Ok(font) => {
            faces.push(font);
            names.push("unifont_csur-17.0.04 (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    (faces, names)
}

fn decode_embedded_face(name: &'static str, bytes: &'static [u8]) -> Result<FontArc> {
    let ttf = convert_woff2_to_ttf(&mut std::io::Cursor::new(bytes))
        .with_context(|| format!("failed to decompress embedded WOFF2 face: {}", name))?;
    FontArc::try_from_vec(ttf).with_context(|| format!("invalid embedded font face: {}", name))
}

fn try_embedded_face(name: &'static str, bytes: &'static [u8]) -> Option<FontArc> {
    match decode_embedded_face(name, bytes) {
        Ok(f) => Some(f),
        Err(err) => {
            warn!(face = name, error = ?err, "failed to load embedded face");
            None
        }
    }
}

fn has_glyph(font: &FontArc, ch: char) -> bool {
    font.glyph_id(ch).0 != 0
}
