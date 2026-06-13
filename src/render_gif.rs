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

use crate::font::{FontSet, load_font_family};
use crate::render_common::is_box_drawing;
use crossbeam_channel::{Receiver, Sender, bounded};
use gifski::{Settings, progress};
use tracing::debug;

use crate::{
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, RenderOptions, ViewportConfig},
    style::window_bar_dot_metrics,
};

/// Cache key identifying a rasterised glyph outline.
#[derive(Hash, Eq, PartialEq)]
struct GlyphCacheKey {
    /// Index into [`FontSet::fonts`].
    font_idx: u16,
    /// ab_glyph glyph identifier within the face.
    glyph_id: u16,
    /// Uniform px-scale as `f32` bits (we only use uniform scales).
    scale_bits_x: u32,
    scale_bits_y: u32,
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
/// Returns `(scale, cell_w, cell_h, baseline, char_height_px, ascent_px)` where:
/// - `baseline` is the distance from the top of the cell to the alphabetic
///   baseline, adjusted to vertically centre the glyph bbox within the cell
///   (i.e. `raw_ascent + extra / 2` where `extra = cell_h - bbox_h`).
/// - `char_height_px` is the raw font bbox height (`ascent - descent`) before
///   the `line_height` multiplier — the natural glyph height at `font_size`.
/// - `ascent_px` is the raw scaled ascent (before centering).
fn css_cell_metrics(
    font: &FontArc,
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
) -> (PxScale, u32, u32, u32, u32, u32) {
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
    // xterm.js: cell_w = round(charWidth + letterSpacing)
    // letterSpacing is additive pixels (default 1.0), not a multiplier.
    let cell_w = (scaled.h_advance(font.glyph_id('M')) + letter_spacing)
        .round()
        .max(1.0) as u32;
    // xterm.js: cell_h = ceil(bboxHeight * lineHeight)
    // lineHeight is a multiplier on the font's actual bounding box height
    // (ascent - descent), not on font_size. For JetBrains Mono at 22px the
    // bbox is ~27px; font_size alone would give 22px — too tight.
    let bbox_h = scaled.ascent() - scaled.descent();
    let cell_h = (bbox_h * line_height).ceil().max(1.0) as u32;
    let char_height_px = bbox_h.round().max(0.0) as u32;
    let raw_ascent = scaled.ascent().round().max(0.0) as u32;
    // Centre the glyph bbox vertically in the cell: push the baseline down by
    // half the extra space introduced by line_height > 1.
    let extra = (cell_h as f32 - bbox_h).max(0.0);
    let baseline = (scaled.ascent() + extra / 2.0).round().max(0.0) as u32;
    (scale, cell_w, cell_h, baseline, char_height_px, raw_ascent)
}

/// Load the font family for `font_path` (or the embedded default) and return
/// `(cell_w_px, cell_h_px)` using the same CSS em-square semantics as
/// [`css_cell_metrics`].
///
/// Called by the runner to derive the terminal grid size before any renderer
/// is spawned, so cols/rows are computed from the actual font metrics rather
/// than a geometric approximation.
/// Returns `(cell_w, cell_h, char_height_px, ascent_px)` using the same CSS
/// em-square semantics as [`css_cell_metrics`].
///
/// `char_height_px` is the raw font bbox height (ascent − descent) without
/// the `line_height` multiplier.  `ascent_px` is the raw scaled ascent.
/// These are propagated into [`ViewportConfig`] so downstream renderers (e.g.
/// SVG) can compute a centred baseline without reloading the font.
pub fn measure_cell_px(
    font_path: Option<&str>,
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
) -> (u32, u32, u32, u32) {
    let font_set = load_font_family(font_path)
        .expect("font metrics are always required but font failed to load")
        .font_set;
    let primary = &font_set.fonts[font_set.regular[0]];
    let (_scale, cell_w, cell_h, _baseline, char_height_px, ascent_px) =
        css_cell_metrics(&primary.font, font_size, line_height, letter_spacing);
    (cell_w, cell_h, char_height_px, ascent_px)
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
    let (scale, _, _, baseline, char_height_px, ascent_px) = css_cell_metrics(
        &primary.font,
        opts.font_size,
        opts.line_height,
        opts.letter_spacing,
    );
    let mut cfg = cfg;
    cfg.font_size_px = opts.font_size;
    cfg.char_height_px = char_height_px;
    cfg.ascent_px = ascent_px;

    let no_system_fonts = opts.no_system_fonts;
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RAW_FRAME_CONSUMER_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-gif-stream".into())
        .spawn(move || {
            run_gif_stream_worker(rx, output, font_set, scale, baseline, cfg, no_system_fonts)
        })
        .expect("failed to spawn gif stream worker");

    Ok(GifStreamHandle { tx, join })
}

pub fn render_gif(rec: &Recording, opts: &RenderOptions, out: &Path) -> Result<()> {
    let stream = spawn_gif_stream(
        ViewportConfig::new(
            rec.cols,
            rec.rows,
            rec.framerate,
            rec.cell_width_px,
            rec.cell_height_px,
            rec.frame_style,
            rec.font_size_px,
            rec.char_height_px,
            rec.ascent_px,
            rec.letter_spacing,
        ),
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
    let (scale, cell_w, cell_h, baseline, char_height_px, ascent_px) = css_cell_metrics(
        &primary.font,
        opts.font_size,
        opts.line_height,
        opts.letter_spacing,
    );
    let cfg = ViewportConfig::new(
        frame.cols,
        frame.rows,
        0,
        cell_w,
        cell_h,
        opts.frame_style,
        opts.font_size,
        char_height_px,
        ascent_px,
        opts.letter_spacing,
    );
    let mut glyph_cache = GlyphCache::new();
    if opts.no_system_fonts {
        for cell in &frame.cells {
            for ch in cell.text.chars() {
                let (_, font) = font_set.select_for_char(cell.flags, ch);
                if font.glyph_id(ch).0 == 0 {
                    return Err(anyhow!(
                        "Glyph not found in embedded fonts for character '{}' (U+{:04X})",
                        ch,
                        ch as u32
                    ));
                }
            }
        }
    }
    let buf = rasterize_raw_frame(frame, &font_set, scale, baseline, cfg, &mut glyph_cache);
    lodepng::encode24_file(out, &buf, cfg.canvas_w as usize, cfg.canvas_h as usize)
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
    baseline: u32,
    cfg: ViewportConfig,
    no_system_fonts: bool,
) -> Result<()> {
    let (collector, writer) = gifski::new(Settings {
        width: Some(cfg.canvas_w),
        height: Some(cfg.canvas_h),
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
        if no_system_fonts {
            for cell in &frame.cells {
                for ch in cell.text.chars() {
                    let (_, font) = font_set.select_for_char(cell.flags, ch);
                    if font.glyph_id(ch).0 == 0 {
                        return Err(anyhow!(
                            "Glyph not found in embedded fonts for character '{}' (U+{:04X})",
                            ch,
                            ch as u32
                        ));
                    }
                }
            }
        }
        let buf = rasterize_raw_frame(&frame, &font_set, scale, baseline, cfg, &mut glyph_cache);

        last_seen_t_ms = frame.t_ms;

        if prev_buf.is_none() {
            // gifski expects absolute presentation timestamps and the first
            // frame at t=0. Emit the very first captured frame unconditionally
            // so leading sleeps are represented correctly.
            let rgba = rgb_to_rgba(&buf);
            let frame_img = imgref::ImgVec::new(rgba, cfg.canvas_w as usize, cfg.canvas_h as usize);
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
        let frame_img = imgref::ImgVec::new(rgba, cfg.canvas_w as usize, cfg.canvas_h as usize);
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
        let frame_img = imgref::ImgVec::new(rgba, cfg.canvas_w as usize, cfg.canvas_h as usize);
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
    baseline: u32,
    cfg: ViewportConfig,
    glyph_cache: &mut GlyphCache,
) -> Vec<u8> {
    let cell_w = cfg.cell_width_px.max(1);
    let cell_h = cfg.cell_height_px.max(1);
    let mut buf = vec![0u8; (cfg.canvas_w * cfg.canvas_h * 3) as usize];

    fill_rect(
        &mut buf,
        cfg.canvas_w,
        0,
        0,
        cfg.canvas_w,
        cfg.canvas_h,
        cfg.frame_style.margin_fill,
    );
    fill_rect(
        &mut buf,
        cfg.canvas_w,
        cfg.frame_x,
        cfg.frame_y,
        cfg.frame_w,
        cfg.frame_h,
        frame.default_bg,
    );
    if cfg.frame_style.window_bar.enabled() {
        draw_window_bar(&mut buf, cfg.canvas_w, cfg);
    }

    for row in 0..frame.rows {
        for col in 0..frame.cols {
            let idx = row as usize * frame.cols as usize + col as usize;
            let cell = &frame.cells[idx];
            let x = cfg.content_x + col as u32 * cell_w;
            let y = cfg.content_y + row as u32 * cell_h;

            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & style_flags::INVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            // SGR 2 dim: blend fg 50% toward bg (equivalent to opacity 0.5).
            if cell.flags & style_flags::DIM != 0 {
                fg = dim_color(fg, bg);
            }

            let is_cursor = frame.cursor == Some((col, row));
            if is_cursor {
                bg = frame.cursor_color.unwrap_or(frame.default_fg);
                fg = frame.cursor_accent.unwrap_or(frame.default_bg);
            }

            if bg != frame.default_bg || cell.flags & style_flags::INVERSE != 0 || is_cursor {
                fill_rect(&mut buf, cfg.canvas_w, x, y, cell_w, cell_h, bg);
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
                let mut glyph_scale = scale;
                let mut char_baseline = pen_y_baseline;

                if is_box_drawing(ch) {
                    let scaled = font.as_scaled(scale);
                    let advance = scaled.h_advance(glyph_id);
                    let bbox_w = cell_w as f32;
                    let bbox_h = cell_h as f32;

                    let glyph_w = advance.max(1.0);
                    let glyph_h = (scaled.ascent() - scaled.descent()).max(1.0);

                    // We want the box drawing character to exactly fill the cell width and height,
                    // so we stretch it accordingly.
                    glyph_scale.x = scale.x * (bbox_w / glyph_w);
                    glyph_scale.y = scale.y * (bbox_h / glyph_h);

                    let stretched_scaled = font.as_scaled(glyph_scale);
                    char_baseline = (y as f32 + stretched_scaled.ascent()).round() as i32;
                }

                let cache_key = GlyphCacheKey {
                    font_idx: font_idx as u16,
                    glyph_id: glyph_id.0,
                    scale_bits_x: glyph_scale.x.to_bits(),
                    scale_bits_y: glyph_scale.y.to_bits(),
                };
                let bitmap = glyph_cache.entry(cache_key).or_insert_with(|| {
                    let glyph: Glyph = glyph_id.with_scale(glyph_scale);
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
                    let mut draw_pen_x = pen_x;
                    if !is_box_drawing(ch) {
                        draw_pen_x += (cfg.letter_spacing / 2.0).floor();
                    }
                    for gy in 0..bm.height {
                        for gx in 0..bm.width {
                            let coverage = bm.pixels[(gy * bm.width + gx) as usize];
                            if coverage <= 0.0 {
                                continue;
                            }
                            let px = draw_pen_x as i32 + bm.offset_x + gx as i32;
                            let py = char_baseline + bm.offset_y + gy as i32;
                            if px < 0 || py < 0 {
                                continue;
                            }
                            let (px, py) = (px as u32, py as u32);
                            if px >= cfg.canvas_w || py >= cfg.canvas_h {
                                continue;
                            }
                            blend_pixel(&mut buf, cfg.canvas_w, px, py, fg, coverage);
                        }
                    }
                }

                let scaled = font.as_scaled(scale);
                pen_x += scaled.h_advance(glyph_id);
            }

            if cell.flags & style_flags::UNDERLINE != 0 {
                let uy = y + cell_h.saturating_sub(2);
                fill_rect(&mut buf, cfg.canvas_w, x, uy, cell_w, 1, fg);
            }
        }
    }

    // Render mouse cursor if visible in the frame
    if let Some((m_col, m_row, m_state)) = frame.mouse_cursor {
        use crate::recording::MouseState;
        let cx = (cfg.content_x + m_col as u32 * cell_w + cell_w / 2) as i32;
        let cy = (cfg.content_y + m_row as u32 * cell_h + cell_h / 2) as i32;

        // Draw click/drag visual ripple circle under the pointer
        match m_state {
            MouseState::Clicking => {
                draw_circle(
                    &mut buf,
                    cfg.canvas_w,
                    cfg.canvas_h,
                    cx,
                    cy,
                    16,
                    [255, 0, 0],
                    0.5,
                );
            }
            MouseState::Dragging => {
                draw_circle(
                    &mut buf,
                    cfg.canvas_w,
                    cfg.canvas_h,
                    cx,
                    cy,
                    16,
                    [237, 97, 215],
                    0.5,
                );
            }
            MouseState::Moving => {}
        }

        // Draw cursor diagonal arrow pointer on top (scaled 2x)
        for dy in 0..CURSOR_HEIGHT {
            for dx in 0..CURSOR_WIDTH {
                let val = CURSOR_BITMAP[(dy * CURSOR_WIDTH + dx) as usize];
                if val == 0 {
                    continue;
                }
                let color = if val == 1 { [255, 255, 255] } else { [0, 0, 0] };
                for sy in 0..2 {
                    for sx in 0..2 {
                        let px = cx + (dx as i32 * 2) + sx;
                        let py = cy + (dy as i32 * 2) + sy;
                        if px >= 0
                            && px < cfg.canvas_w as i32
                            && py >= 0
                            && py < cfg.canvas_h as i32
                        {
                            blend_pixel(&mut buf, cfg.canvas_w, px as u32, py as u32, color, 1.0);
                        }
                    }
                }
            }
        }
    }

    if cfg.frame_style.border_radius_px > 0 {
        mask_outside_rounded_rect(
            &mut buf,
            cfg.canvas_w,
            cfg,
            cfg.frame_style.border_radius_px,
            cfg.frame_style.margin_fill,
        );
    }

    buf
}

fn draw_window_bar(buf: &mut [u8], w: u32, cfg: ViewportConfig) {
    let bar_h = cfg.bar_h;
    let (radius, gap) = window_bar_dot_metrics(bar_h);
    let dots_w = radius * 2 * 3 + gap * 2;
    let style = cfg.frame_style.window_bar;
    let start_x = if style.align_right() {
        cfg.frame_x + cfg.frame_w.saturating_sub(dots_w + gap)
    } else {
        cfg.frame_x + gap
    };
    let cy = cfg.frame_y + bar_h / 2;
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
    cfg: ViewportConfig,
    radius: u32,
    fill: [u8; 3],
) {
    let radius = radius.min(cfg.frame_w / 2).min(cfg.frame_h / 2) as i64;
    for y in cfg.frame_y..cfg.frame_y + cfg.frame_h {
        for x in cfg.frame_x..cfg.frame_x + cfg.frame_w {
            if !inside_rounded_rect(x, y, cfg, radius) {
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

fn inside_rounded_rect(x: u32, y: u32, cfg: ViewportConfig, radius: i64) -> bool {
    if radius == 0 {
        return true;
    }
    let x = x as i64;
    let y = y as i64;
    let left = cfg.frame_x as i64;
    let top = cfg.frame_y as i64;
    let right = (cfg.frame_x + cfg.frame_w - 1) as i64;
    let bottom = (cfg.frame_y + cfg.frame_h - 1) as i64;
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

/// SGR 2 dim: blend foreground 50% toward background (opacity 0.5 equivalent).
fn dim_color(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    [
        ((fg[0] as u16 + bg[0] as u16) / 2) as u8,
        ((fg[1] as u16 + bg[1] as u16) / 2) as u8,
        ((fg[2] as u16 + bg[2] as u16) / 2) as u8,
    ]
}

const CURSOR_WIDTH: u32 = 12;
const CURSOR_HEIGHT: u32 = 19;

// 1 represents cursor fill (white), 2 represents cursor border (black), 0 is transparent
const CURSOR_BITMAP: [u8; 12 * 19] = [
    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 2, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 2, 1, 1, 1,
    1, 2, 0, 0, 0, 0, 0, 0, 2, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 2, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0,
    2, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 2, 1, 1, 1, 1, 1, 2, 2,
    2, 2, 2, 0, 2, 1, 1, 2, 1, 1, 2, 0, 0, 0, 0, 0, 2, 1, 2, 0, 2, 1, 1, 2, 0, 0, 0, 0, 2, 2, 0, 0,
    2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0,
];

fn draw_circle(
    buf: &mut [u8],
    w: u32,
    canvas_h: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 3],
    opacity: f32,
) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                let px = cx + dx;
                let py = cy + dy;
                if px >= 0 && px < w as i32 && py >= 0 && py < canvas_h as i32 {
                    blend_pixel(buf, w, px as u32, py as u32, color, opacity);
                }
            }
        }
    }
}
