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
use tracing::debug;

use crate::{
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, RenderOptions, ViewportConfig},
    style::window_bar_dot_metrics,
};

const TRANSPARENT_COLOR_INDEX: u8 = 255;

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
fn css_cell_metrics(
    font: &FontArc,
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
) -> (PxScale, u32, u32, u32, u32, u32) {
    let upem = font
        .units_per_em()
        .unwrap_or_else(|| font.height_unscaled());
    let height_units = font.height_unscaled().max(1.0);
    let px_scale = font_size * height_units / upem;
    let scale = PxScale::from(px_scale);
    let scaled = font.as_scaled(scale);
    let cell_w = (scaled.h_advance(font.glyph_id('M')) + letter_spacing)
        .round()
        .max(1.0) as u32;
    let bbox_h = scaled.ascent() - scaled.descent();
    let cell_h = (bbox_h * line_height).ceil().max(1.0) as u32;
    let char_height_px = bbox_h.round().max(0.0) as u32;
    let raw_ascent = scaled.ascent().round().max(0.0) as u32;
    let extra = (cell_h as f32 - bbox_h).max(0.0);
    let baseline = (scaled.ascent() + extra / 2.0).round().max(0.0) as u32;
    (scale, cell_w, cell_h, baseline, char_height_px, raw_ascent)
}

pub fn measure_cell_px(
    font_path: Option<&str>,
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
) -> (u32, u32, u32, u32) {
    let font_set = {
        let _timer = crate::telemetry::ScopeTimer::new("measure_cell_px_font_load");
        load_font_family(font_path)
            .expect("font metrics are always required but font failed to load")
            .font_set
    };
    let _timer_metrics = crate::telemetry::ScopeTimer::new("measure_cell_px_metrics");
    let primary = &font_set.fonts[font_set.regular[0]];
    let (_scale, cell_w, cell_h, _baseline, char_height_px, ascent_px) =
        css_cell_metrics(primary.get_font(), font_size, line_height, letter_spacing);
    (cell_w, cell_h, char_height_px, ascent_px)
}

pub fn spawn_gif_stream(
    cfg: ViewportConfig,
    opts: RenderOptions,
    output: PathBuf,
) -> Result<GifStreamHandle> {
    let loaded = {
        let _timer = crate::telemetry::ScopeTimer::new("spawn_gif_stream_font_load");
        load_font_family(opts.font_path.as_deref())?
    };
    debug!(font = %loaded.description, "using font for gif streaming");

    let _timer_setup = crate::telemetry::ScopeTimer::new("spawn_gif_stream_setup");
    let font_set = loaded.font_set;
    let primary = &font_set.fonts[font_set.regular[0]];
    let (scale, _, _, baseline, char_height_px, ascent_px) = css_cell_metrics(
        primary.get_font(),
        opts.font_size,
        opts.line_height,
        opts.letter_spacing,
    );
    let mut cfg = cfg;
    cfg.font_size_px = opts.font_size;
    cfg.char_height_px = char_height_px;
    cfg.ascent_px = ascent_px;

    let no_system_fonts = opts.no_system_fonts;
    let theme = opts.theme.clone();
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
                baseline,
                cfg,
                no_system_fonts,
                theme,
            )
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
        primary.get_font(),
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
    let palette = generate_256_palette(
        opts.theme.palette_rgb()?,
        opts.theme.background_rgb()?,
        opts.theme.foreground_rgb()?,
    );
    let mut index_buf = vec![TRANSPARENT_COLOR_INDEX; (cfg.canvas_w * cfg.canvas_h) as usize];
    rasterize_raw_frame_idx(
        &mut index_buf,
        None,
        frame,
        None,
        &font_set,
        scale,
        baseline,
        cfg,
        &mut glyph_cache,
        &palette,
    );
    let mut rgb_buf = vec![0u8; (cfg.canvas_w * cfg.canvas_h * 3) as usize];
    for i in 0..index_buf.len() {
        let idx = index_buf[i];
        let color = if idx == TRANSPARENT_COLOR_INDEX {
            frame.default_bg
        } else {
            palette[idx as usize]
        };
        rgb_buf[i * 3] = color[0];
        rgb_buf[i * 3 + 1] = color[1];
        rgb_buf[i * 3 + 2] = color[2];
    }
    lodepng::encode24_file(out, &rgb_buf, cfg.canvas_w as usize, cfg.canvas_h as usize)
        .with_context(|| format!("encoding {}", out.display()))
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
    theme: crate::Theme,
) -> Result<()> {
    let _worker_timer = crate::telemetry::ScopeTimer::new("gif_worker_total");
    let start_time = std::time::Instant::now();

    let base16 = theme.palette_rgb()?;
    let bg = theme.background_rgb()?;
    let fg = theme.foreground_rgb()?;
    let palette = generate_256_palette(base16, bg, fg);

    let mut flattened_palette = [0u8; 768];
    for i in 0..256 {
        flattened_palette[i * 3] = palette[i][0];
        flattened_palette[i * 3 + 1] = palette[i][1];
        flattened_palette[i * 3 + 2] = palette[i][2];
    }

    let file = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    let mut encoder = gif::Encoder::new(
        file,
        cfg.canvas_w as u16,
        cfg.canvas_h as u16,
        &flattened_palette,
    )
    .context("initialize gif encoder")?;

    encoder
        .set_repeat(gif::Repeat::Infinite)
        .context("set gif repeat")?;

    let mut glyph_cache = GlyphCache::new();
    let mut last_seen_t_ms = 0u32;
    let mut pending_frame: Option<(Vec<u8>, u32)> = None;
    let mut prev_frame: Option<RawFrame> = None;
    let mut frame_index = 0usize;

    let mut canvas_buf = vec![TRANSPARENT_COLOR_INDEX; (cfg.canvas_w * cfg.canvas_h) as usize];
    let mut prev_buf: Option<Vec<u8>> = None;

    while let Ok(frame) = rx.recv() {
        if let Some(ref prev) = prev_frame {
            if frame.is_visually_identical(prev) {
                last_seen_t_ms = frame.t_ms;
                continue;
            }
        }

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

        if prev_frame.is_some() {
            canvas_buf.fill(TRANSPARENT_COLOR_INDEX);
        }

        {
            let _rast_timer = crate::telemetry::ScopeTimer::new("gif_rasterize_frame");
            rasterize_raw_frame_idx(
                &mut canvas_buf,
                prev_buf.as_deref(),
                &frame,
                prev_frame.as_ref(),
                &font_set,
                scale,
                baseline,
                cfg,
                &mut glyph_cache,
                &palette,
            );
        }

        last_seen_t_ms = frame.t_ms;

        if let Some((pending_buf, pending_t_ms)) = pending_frame.take() {
            let delay_ms = frame.t_ms.saturating_sub(pending_t_ms);
            {
                let _write_timer = crate::telemetry::ScopeTimer::new("gif_write_frame");
                write_gif_frame(&mut encoder, &pending_buf, delay_ms, cfg)?;
            }
            frame_index += 1;
            prev_buf = Some(pending_buf);
        }

        pending_frame = Some((canvas_buf.clone(), frame.t_ms));
        prev_frame = Some(frame);
    }

    if let Some((pending_buf, pending_t_ms)) = pending_frame.take() {
        let delay_ms = last_seen_t_ms.saturating_sub(pending_t_ms).max(33);
        {
            let _write_timer = crate::telemetry::ScopeTimer::new("gif_write_frame");
            write_gif_frame(&mut encoder, &pending_buf, delay_ms, cfg)?;
        }
        frame_index += 1;
    }

    drop(encoder);

    {
        let _finalize_timer = crate::telemetry::ScopeTimer::new("gif_finalize_comment");
        let elapsed_ms = start_time.elapsed().as_millis();
        if let Ok(mut data) = std::fs::read(&out) {
            if data.last() == Some(&0x3B) {
                data.pop();
                let comment_str =
                    format!("Created by evp | elapsed_ms={elapsed_ms} frames={frame_index}");
                let comment_bytes = comment_str.as_bytes();
                data.push(0x21); // Extension Introducer
                data.push(0xFE); // Comment Label
                let len = comment_bytes.len().min(255);
                data.push(len as u8);
                data.extend_from_slice(&comment_bytes[..len]);
                data.push(0x00); // Block Terminator
                data.push(0x3B); // Restore GIF Trailer
                let _ = std::fs::write(&out, data);
            }
        }
    }

    Ok(())
}

fn is_cell_changed(
    col: usize,
    row: usize,
    curr: &RawFrame,
    prev: Option<&RawFrame>,
    cfg: ViewportConfig,
    cell_w: u32,
    cell_h: u32,
) -> bool {
    let Some(prev) = prev else {
        return true;
    };

    let idx = row * curr.cols as usize + col;
    let prev_idx = row * prev.cols as usize + col;
    if idx >= curr.cells.len() || prev_idx >= prev.cells.len() {
        return true;
    }
    if curr.cells[idx] != prev.cells[prev_idx] {
        return true;
    }

    let curr_cursor = curr.cursor == Some((col as u16, row as u16));
    let prev_cursor = prev.cursor == Some((col as u16, row as u16));
    if curr_cursor != prev_cursor {
        return true;
    }

    for mouse in &[curr.mouse_cursor, prev.mouse_cursor] {
        if let Some((m_col, m_row, _)) = *mouse {
            let cx = (cfg.content_x as f32 + m_col * cell_w as f32 + cell_w as f32 / 2.0) as i32;
            let cy = (cfg.content_y as f32 + m_row * cell_h as f32 + cell_h as f32 / 2.0) as i32;

            let x_min = cx - 48;
            let x_max = cx + 48;
            let y_min = cy - 48;
            let y_max = cy + 48;

            let cell_x = (cfg.content_x + col as u32 * cell_w) as i32;
            let cell_y = (cfg.content_y + row as u32 * cell_h) as i32;

            let overlap_x = cell_x < x_max && cell_x + cell_w as i32 > x_min;
            let overlap_y = cell_y < y_max && cell_y + cell_h as i32 > y_min;
            if overlap_x && overlap_y {
                return true;
            }
        }
    }

    false
}

#[allow(clippy::too_many_arguments)]
fn rasterize_raw_frame_idx(
    buf: &mut [u8],
    prev_buf: Option<&[u8]>,
    curr: &RawFrame,
    prev: Option<&RawFrame>,
    font_set: &FontSet,
    scale: PxScale,
    baseline: u32,
    cfg: ViewportConfig,
    glyph_cache: &mut GlyphCache,
    palette: &[[u8; 3]; 256],
) {
    let cell_w = cfg.cell_width_px.max(1);
    let cell_h = cfg.cell_height_px.max(1);

    let default_bg_idx = find_closest_color(curr.default_bg, palette);
    let margin_idx = find_closest_color(cfg.frame_style.margin_fill, palette);

    if prev.is_none() {
        fill_rect_idx(
            buf,
            cfg.canvas_w,
            0,
            0,
            cfg.canvas_w,
            cfg.canvas_h,
            margin_idx,
        );
        fill_rect_idx(
            buf,
            cfg.canvas_w,
            cfg.frame_x,
            cfg.frame_y,
            cfg.frame_w,
            cfg.frame_h,
            default_bg_idx,
        );
    }

    let title_changed = prev.is_none() || curr.title != prev.unwrap().title;
    if title_changed {
        if cfg.frame_style.window_bar.enabled() {
            fill_rect_idx(
                buf,
                cfg.canvas_w,
                cfg.frame_x,
                cfg.frame_y,
                cfg.frame_w,
                cfg.bar_h,
                default_bg_idx,
            );
            draw_window_bar_idx(buf, cfg.canvas_w, cfg, palette);
            if let Some(ref title) = curr.title {
                if !title.is_empty() {
                    let title_fs = (cfg.bar_h as f32 * 0.765).max(17.0);
                    let title_scale = PxScale::from(title_fs);
                    let text_w = string_width(title, font_set, title_scale);

                    let cx = cfg.frame_x as f32 + cfg.frame_w as f32 / 2.0;
                    let start_x = (cx - text_w / 2.0).max(cfg.frame_x as f32);

                    let cy = cfg.frame_y as f32 + cfg.bar_h as f32 / 2.0;
                    let (_, font) = font_set.select_for_char(0, 'M');
                    let scaled = font.as_scaled(title_scale);
                    let baseline_y = cy + (scaled.ascent() + scaled.descent()) / 2.0;

                    draw_string_idx(
                        buf,
                        prev_buf,
                        palette,
                        cfg.canvas_w,
                        start_x as u32,
                        baseline_y as u32,
                        title,
                        font_set,
                        title_scale,
                        [142, 142, 147],
                        glyph_cache,
                        curr.default_bg,
                    );
                }
            }
        }
    }

    for row in 0..curr.rows as usize {
        for col in 0..curr.cols as usize {
            if is_cell_changed(col, row, curr, prev, cfg, cell_w, cell_h) {
                let x = cfg.content_x + col as u32 * cell_w;
                let y = cfg.content_y + row as u32 * cell_h;

                let idx = row * curr.cols as usize + col;
                let cell = &curr.cells[idx];

                let (mut fg, mut bg) = (cell.fg, cell.bg);
                if cell.flags & style_flags::INVERSE != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags & style_flags::DIM != 0 {
                    fg = dim_color(fg, bg);
                }

                let is_cursor = curr.cursor == Some((col as u16, row as u16));
                if is_cursor {
                    bg = curr.cursor_color.unwrap_or(curr.default_fg);
                    fg = curr.cursor_accent.unwrap_or(curr.default_bg);
                }

                let bg_idx = find_closest_color(bg, palette);
                fill_rect_idx(buf, cfg.canvas_w, x, y, cell_w, cell_h, bg_idx);

                if !cell.text.is_empty() {
                    let mut pen_x = x as f32;
                    let pen_y_baseline = y as i32 + baseline as i32;
                    for ch in cell.text.chars() {
                        let (font_idx, font) = font_set.select_for_char(cell.flags, ch);
                        let glyph_id: GlyphId = font.glyph_id(ch);

                        let is_primary = Some(font_idx) == font_set.regular.first().copied()
                            || Some(font_idx) == font_set.bold.first().copied()
                            || Some(font_idx) == font_set.italic.first().copied()
                            || Some(font_idx) == font_set.bold_italic.first().copied();

                        let glyph_scale = if is_primary {
                            scale
                        } else {
                            let cell_ratio = cell_w as f32 / cell_h as f32;
                            let (_, font_ref) = font_set.select_for_char(0, 'M');
                            let scaled_ref = font_ref.as_scaled(scale);
                            let ref_h = scaled_ref.ascent() - scaled_ref.descent();
                            let font_scaled = font.as_scaled(scale);
                            let glyph_h = font_scaled.ascent() - font_scaled.descent();
                            let scale_factor = (ref_h / glyph_h) * cell_ratio;
                            PxScale::from(scale.x * scale_factor)
                        };

                        let char_baseline = if is_primary {
                            pen_y_baseline
                        } else {
                            let font_scaled = font.as_scaled(glyph_scale);
                            let cell_center_y = y as f32 + cell_h as f32 / 2.0;
                            let glyph_center_y =
                                (font_scaled.ascent() + font_scaled.descent()) / 2.0;
                            (cell_center_y + glyph_center_y) as i32
                        };

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
                                let w = bounds.width().round() as u32;
                                let h = bounds.height().round() as u32;
                                let mut pixels = vec![0.0f32; (w * h) as usize];
                                outline.draw(|gx, gy, coverage| {
                                    let i = (gy * w + gx) as usize;
                                    if i < pixels.len() {
                                        pixels[i] = coverage;
                                    }
                                });
                                GlyphBitmap {
                                    offset_x: bounds.min.x.round() as i32,
                                    offset_y: bounds.min.y.round() as i32,
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
                                    blend_pixel_idx(
                                        buf,
                                        prev_buf,
                                        palette,
                                        cfg.canvas_w,
                                        px,
                                        py,
                                        fg,
                                        coverage,
                                        curr.default_bg,
                                    );
                                }
                            }
                        }

                        let scaled = font.as_scaled(scale);
                        pen_x += scaled.h_advance(glyph_id);
                    }
                }

                if cell.flags & style_flags::UNDERLINE != 0 {
                    let uy = y + cell_h.saturating_sub(2);
                    let fg_idx = find_closest_color(fg, palette);
                    fill_rect_idx(buf, cfg.canvas_w, x, uy, cell_w, 1, fg_idx);
                }
            }
        }
    }

    if let Some((m_col, m_row, m_state)) = curr.mouse_cursor {
        use crate::recording::MouseState;
        let cx = (cfg.content_x as f32 + m_col * cell_w as f32 + cell_w as f32 / 2.0) as i32;
        let cy = (cfg.content_y as f32 + m_row * cell_h as f32 + cell_h as f32 / 2.0) as i32;

        match m_state {
            MouseState::Clicking => {
                draw_circle_idx(
                    buf,
                    prev_buf,
                    palette,
                    cfg.canvas_w,
                    cfg.canvas_h,
                    cx,
                    cy,
                    16,
                    [255, 0, 0],
                    0.5,
                    curr.default_bg,
                );
            }
            MouseState::Dragging => {
                draw_circle_idx(
                    buf,
                    prev_buf,
                    palette,
                    cfg.canvas_w,
                    cfg.canvas_h,
                    cx,
                    cy,
                    16,
                    [237, 97, 215],
                    0.5,
                    curr.default_bg,
                );
            }
            MouseState::Moving => {}
        }

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
                            blend_pixel_idx(
                                buf,
                                prev_buf,
                                palette,
                                cfg.canvas_w,
                                px as u32,
                                py as u32,
                                color,
                                1.0,
                                curr.default_bg,
                            );
                        }
                    }
                }
            }
        }
    }

    if prev.is_none() && cfg.frame_style.border_radius_px > 0 {
        mask_outside_rounded_rect_idx(
            buf,
            cfg.canvas_w,
            cfg,
            cfg.frame_style.border_radius_px,
            margin_idx,
        );
    }
}

fn dim_color(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    [
        ((fg[0] as u16 + bg[0] as u16) / 2) as u8,
        ((fg[1] as u16 + bg[1] as u16) / 2) as u8,
        ((fg[2] as u16 + bg[2] as u16) / 2) as u8,
    ]
}

const CURSOR_WIDTH: u32 = 12;
const CURSOR_HEIGHT: u32 = 19;

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

fn fill_rect_idx(buf: &mut [u8], w: u32, x: u32, y: u32, width: u32, height: u32, idx: u8) {
    for row in y..y + height {
        let start = (row * w + x) as usize;
        if start + width as usize <= buf.len() {
            buf[start..start + width as usize].fill(idx);
        }
    }
}

fn blend_pixel_idx(
    buf: &mut [u8],
    prev_buf: Option<&[u8]>,
    palette: &[[u8; 3]; 256],
    w: u32,
    x: u32,
    y: u32,
    color: [u8; 3],
    alpha: f32,
    default_bg: [u8; 3],
) {
    let idx = (y * w + x) as usize;
    if idx >= buf.len() {
        return;
    }
    let mut bg_idx = buf[idx];
    if bg_idx == TRANSPARENT_COLOR_INDEX {
        if let Some(prev) = prev_buf {
            if idx < prev.len() {
                bg_idx = prev[idx];
            }
        }
    }
    let bg = if bg_idx == TRANSPARENT_COLOR_INDEX {
        default_bg
    } else {
        palette[bg_idx as usize]
    };
    let r = color[0] as f32 * alpha + bg[0] as f32 * (1.0 - alpha);
    let g = color[1] as f32 * alpha + bg[1] as f32 * (1.0 - alpha);
    let b = color[2] as f32 * alpha + bg[2] as f32 * (1.0 - alpha);
    let blended = [
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    ];
    buf[idx] = find_closest_color(blended, palette);
}

fn string_width(text: &str, font_set: &FontSet, scale: PxScale) -> f32 {
    let mut w = 0.0;
    for ch in text.chars() {
        let (_, font) = font_set.select_for_char(0, ch);
        let glyph_id = font.glyph_id(ch);
        let scaled = font.as_scaled(scale);
        w += scaled.h_advance(glyph_id);
    }
    w
}

fn draw_string_idx(
    buf: &mut [u8],
    prev_buf: Option<&[u8]>,
    palette: &[[u8; 3]; 256],
    w: u32,
    x: u32,
    y: u32,
    text: &str,
    font_set: &FontSet,
    scale: PxScale,
    color: [u8; 3],
    glyph_cache: &mut GlyphCache,
    default_bg: [u8; 3],
) {
    let mut pen_x = x as f32;
    for ch in text.chars() {
        let (font_idx, font) = font_set.select_for_char(0, ch);
        let glyph_id = font.glyph_id(ch);
        let scaled = font.as_scaled(scale);

        let cache_key = GlyphCacheKey {
            font_idx: font_idx as u16,
            glyph_id: glyph_id.0,
            scale_bits_x: scale.x.to_bits(),
            scale_bits_y: scale.y.to_bits(),
        };

        let bitmap = glyph_cache.entry(cache_key).or_insert_with(|| {
            let glyph = glyph_id.with_scale(scale);
            font.outline_glyph(glyph).map(|outline| {
                let bounds = outline.px_bounds();
                let width = bounds.width().round() as u32;
                let height = bounds.height().round() as u32;
                let mut pixels = vec![0.0f32; (width * height) as usize];
                outline.draw(|x, y, c| {
                    let idx = (y * width + x) as usize;
                    if idx < pixels.len() {
                        pixels[idx] = c;
                    }
                });
                GlyphBitmap {
                    offset_x: bounds.min.x.round() as i32,
                    offset_y: bounds.min.y.round() as i32,
                    width,
                    height,
                    pixels,
                }
            })
        });

        if let Some(bm) = bitmap.as_ref() {
            let mut draw_pen_x = pen_x;
            if !is_box_drawing(ch) {
                draw_pen_x += (scaled.h_advance(glyph_id) - bm.width as f32) / 2.0;
            }
            for gy in 0..bm.height {
                for gx in 0..bm.width {
                    let coverage = bm.pixels[(gy * bm.width + gx) as usize];
                    if coverage <= 0.0 {
                        continue;
                    }
                    let px = draw_pen_x as i32 + bm.offset_x + gx as i32;
                    let py = y as i32 + bm.offset_y + gy as i32;
                    if px < 0 || py < 0 {
                        continue;
                    }
                    let (px, py) = (px as u32, py as u32);
                    if px >= w {
                        continue;
                    }
                    blend_pixel_idx(
                        buf, prev_buf, palette, w, px, py, color, coverage, default_bg,
                    );
                }
            }
        }

        pen_x += scaled.h_advance(glyph_id);
    }
}

fn draw_circle_idx(
    buf: &mut [u8],
    prev_buf: Option<&[u8]>,
    palette: &[[u8; 3]; 256],
    w: u32,
    h: u32,
    cx: i32,
    cy: i32,
    radius: u32,
    color: [u8; 3],
    opacity: f32,
    default_bg: [u8; 3],
) {
    let r = radius as i32;
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let dx = x - cx;
            let dy = y - cy;
            let dist_sq = dx * dx + dy * dy;
            let r_sq = r * r;

            if dist_sq <= r_sq {
                let dist = (dist_sq as f32).sqrt();
                let border_dist = r as f32 - dist;
                let mut alpha = 1.0;
                if border_dist < 1.0 {
                    alpha = border_dist;
                }
                alpha *= opacity;

                if x >= 0 && x < w as i32 && y >= 0 && y < h as i32 {
                    blend_pixel_idx(
                        buf, prev_buf, palette, w, x as u32, y as u32, color, alpha, default_bg,
                    );
                }
            }
        }
    }
}

fn draw_window_bar_idx(buf: &mut [u8], w: u32, cfg: ViewportConfig, palette: &[[u8; 3]; 256]) {
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
        let color_idx = find_closest_color(*color, palette);
        fill_circle_idx(buf, w, cx, cy, radius, color_idx);
    }
}

fn fill_circle_idx(buf: &mut [u8], w: u32, cx: u32, cy: u32, radius: u32, color_idx: u8) {
    let r2 = (radius * radius) as i64;
    for y in cy.saturating_sub(radius)..=cy + radius {
        for x in cx.saturating_sub(radius)..=cx + radius {
            let dx = x as i64 - cx as i64;
            let dy = y as i64 - cy as i64;
            if dx * dx + dy * dy <= r2 {
                let i = (y * w + x) as usize;
                if i < buf.len() {
                    buf[i] = color_idx;
                }
            }
        }
    }
}

fn mask_outside_rounded_rect_idx(
    buf: &mut [u8],
    w: u32,
    cfg: ViewportConfig,
    radius: u32,
    margin_idx: u8,
) {
    let radius = radius.min(cfg.frame_w / 2).min(cfg.frame_h / 2) as i64;
    for y in cfg.frame_y..cfg.frame_y + cfg.frame_h {
        for x in cfg.frame_x..cfg.frame_x + cfg.frame_w {
            if !inside_rounded_rect(x, y, cfg, radius) {
                let i = (y * w + x) as usize;
                if i < buf.len() {
                    buf[i] = margin_idx;
                }
            }
        }
    }
}

fn clamp(low: f32, high: f32, n: f32) -> f32 {
    n.max(low).min(high)
}

fn rgb_to_lab(rgb: [u8; 3]) -> [f32; 3] {
    let r_val = rgb[0] as f32 / 255.0;
    let g_val = rgb[1] as f32 / 255.0;
    let b_val = rgb[2] as f32 / 255.0;

    let r = if r_val <= 0.04045 {
        r_val / 12.92
    } else {
        ((r_val + 0.055) / 1.055).powf(2.4)
    };
    let g = if g_val <= 0.04045 {
        g_val / 12.92
    } else {
        ((g_val + 0.055) / 1.055).powf(2.4)
    };
    let b = if b_val <= 0.04045 {
        b_val / 12.92
    } else {
        ((b_val + 0.055) / 1.055).powf(2.4)
    };

    let x = (r * 0.4124 + g * 0.3576 + b * 0.1805) / 0.95047;
    let y = (r * 0.2126 + g * 0.7152 + b * 0.0722) / 1.0;
    let z = (r * 0.0193 + g * 0.1192 + b * 0.9505) / 1.08883;

    let fx = if x > 0.008856 {
        x.powf(1.0 / 3.0)
    } else {
        7.787 * x + 16.0 / 116.0
    };
    let fy = if y > 0.008856 {
        y.powf(1.0 / 3.0)
    } else {
        7.787 * y + 16.0 / 116.0
    };
    let fz = if z > 0.008856 {
        z.powf(1.0 / 3.0)
    } else {
        7.787 * z + 16.0 / 116.0
    };

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_rgb(lab: [f32; 3]) -> [u8; 3] {
    let l = lab[0];
    let a = lab[1];
    let b = lab[2];

    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;

    let fx3 = fx * fx * fx;
    let fy3 = fy * fy * fy;
    let fz3 = fz * fz * fz;

    let x = if fx3 > 0.008856 {
        fx3
    } else {
        (fx - 16.0 / 116.0) / 7.787
    };
    let y = if fy3 > 0.008856 {
        fy3
    } else {
        (fy - 16.0 / 116.0) / 7.787
    };
    let z = if fz3 > 0.008856 {
        fz3
    } else {
        (fz - 16.0 / 116.0) / 7.787
    };

    let x = x * 0.95047;
    let y = y * 1.0;
    let z = z * 1.08883;

    let r_lin = x * 3.2406 + y * -1.5372 + z * -0.4986;
    let g_lin = x * -0.9689 + y * 1.8758 + z * 0.0415;
    let b_lin = x * 0.0557 + y * -0.2040 + z * 1.0570;

    let r = if r_lin <= 0.0031308 {
        12.92 * r_lin
    } else {
        1.055 * r_lin.powf(1.0 / 2.4) - 0.055
    };
    let g = if g_lin <= 0.0031308 {
        12.92 * g_lin
    } else {
        1.055 * g_lin.powf(1.0 / 2.4) - 0.055
    };
    let b = if b_lin <= 0.0031308 {
        12.92 * b_lin
    } else {
        1.055 * b_lin.powf(1.0 / 2.4) - 0.055
    };

    [
        clamp(0.0, 255.0, r * 255.0 + 0.5) as u8,
        clamp(0.0, 255.0, g * 255.0 + 0.5) as u8,
        clamp(0.0, 255.0, b * 255.0 + 0.5) as u8,
    ]
}

fn lerp_lab(t: f32, lab1: [f32; 3], lab2: [f32; 3]) -> [f32; 3] {
    [
        lab1[0] + t * (lab2[0] - lab1[0]),
        lab1[1] + t * (lab2[1] - lab1[1]),
        lab1[2] + t * (lab2[2] - lab1[2]),
    ]
}

/// Generates a cohesive 256-color palette based on the active terminal theme.
fn generate_256_palette(base16: [[u8; 3]; 16], bg: [u8; 3], fg: [u8; 3]) -> [[u8; 3]; 256] {
    let bg_lab = rgb_to_lab(bg);
    let fg_lab = rgb_to_lab(fg);

    let base8_lab = [
        bg_lab,
        rgb_to_lab(base16[1]),
        rgb_to_lab(base16[2]),
        rgb_to_lab(base16[3]),
        rgb_to_lab(base16[4]),
        rgb_to_lab(base16[5]),
        rgb_to_lab(base16[6]),
        fg_lab,
    ];

    let mut palette = [[0u8; 3]; 256];
    for i in 0..16 {
        palette[i] = base16[i];
    }

    let mut idx = 16;
    for r in 0..6 {
        let c0 = lerp_lab(r as f32 / 5.0, base8_lab[0], base8_lab[1]);
        let c1 = lerp_lab(r as f32 / 5.0, base8_lab[2], base8_lab[3]);
        let c2 = lerp_lab(r as f32 / 5.0, base8_lab[4], base8_lab[5]);
        let c3 = lerp_lab(r as f32 / 5.0, base8_lab[6], base8_lab[7]);
        for g in 0..6 {
            let c4 = lerp_lab(g as f32 / 5.0, c0, c1);
            let c5 = lerp_lab(g as f32 / 5.0, c2, c3);
            for b in 0..6 {
                let c6 = lerp_lab(b as f32 / 5.0, c4, c5);
                if idx < 232 {
                    palette[idx] = lab_to_rgb(c6);
                    idx += 1;
                }
            }
        }
    }

    for i in 0..24 {
        let t = (i as f32 + 1.0) / 25.0;
        let lab = lerp_lab(t, base8_lab[0], base8_lab[7]);
        if idx < TRANSPARENT_COLOR_INDEX as usize {
            palette[idx] = lab_to_rgb(lab);
            idx += 1;
        }
    }

    palette[TRANSPARENT_COLOR_INDEX as usize] = [0, 0, 0];

    palette
}

fn find_closest_color(color: [u8; 3], palette: &[[u8; 3]; 256]) -> u8 {
    let mut min_dist = u32::MAX;
    let mut best_idx = 0;
    for idx in 0..TRANSPARENT_COLOR_INDEX as usize {
        let pc = palette[idx];
        let dr = color[0] as i32 - pc[0] as i32;
        let dg = color[1] as i32 - pc[1] as i32;
        let db = color[2] as i32 - pc[2] as i32;
        let dist = (dr * dr + dg * dg + db * db) as u32;
        if dist < min_dist {
            min_dist = dist;
            best_idx = idx;
        }
    }
    best_idx as u8
}

fn write_gif_frame<W: std::io::Write>(
    encoder: &mut gif::Encoder<W>,
    curr_indices: &[u8],
    delay_ms: u32,
    cfg: ViewportConfig,
) -> Result<()> {
    let mut gif_frame = gif::Frame::default();
    gif_frame.width = cfg.canvas_w as u16;
    gif_frame.height = cfg.canvas_h as u16;
    gif_frame.left = 0;
    gif_frame.top = 0;
    gif_frame.delay = ((delay_ms + 5) / 10).max(1) as u16;
    gif_frame.transparent = Some(TRANSPARENT_COLOR_INDEX);
    gif_frame.dispose = gif::DisposalMethod::Keep;
    gif_frame.buffer = std::borrow::Cow::Borrowed(curr_indices);

    encoder.write_frame(&gif_frame).context("write gif frame")?;
    Ok(())
}
