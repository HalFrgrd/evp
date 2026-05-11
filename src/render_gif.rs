//! Render a [`Recording`] to an animated GIF using gifski with streaming.
//!
//! We rasterise each frame as an RGBA buffer using `ab_glyph`, then stream
//! frames directly to gifski's collector. This allows encoding to happen
//! concurrently with recording, reducing peak memory and latency.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};
use gifski::{Settings, progress};
use tracing::{info, warn};
use woff2_patched::convert_woff2_to_ttf;

use crate::{
    FrameStyle,
    recording::{RawFrame, Recording, style_flags},
    render_common::{RENDER_STREAM_CHANNEL_CAPACITY, RenderOptions},
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

#[derive(Debug)]
struct FontFamily {
    regular: FontArc,
    bold: Option<FontArc>,
    italic: Option<FontArc>,
    bold_italic: Option<FontArc>,
    fallback_regular: Vec<FontArc>,
}

pub struct GifStreamConfig {
    pub cols: u16,
    pub rows: u16,
}

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

pub fn spawn_gif_stream(
    cfg: GifStreamConfig,
    opts: RenderOptions,
    output: PathBuf,
) -> Result<GifStreamHandle> {
    let loaded = load_font_family(opts.font_path.as_deref())?;
    info!(font = %loaded.description, "using font for gif streaming");

    let family = loaded.family;
    let scale = PxScale::from(opts.font_size);
    let scaled = family.regular.as_scaled(scale);
    let cell_w = scaled
        .h_advance(family.regular.glyph_id('M'))
        .ceil()
        .max(1.0) as u32;
    let cell_h = (scaled.height() + scaled.line_gap()).ceil().max(1.0) as u32;
    let baseline = scaled.ascent().ceil() as u32;
    let layout = layout_metrics(cfg.cols, cfg.rows, cell_w, cell_h, opts.frame_style);

    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) = bounded(RENDER_STREAM_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-gif-stream".into())
        .spawn(move || {
            run_gif_stream_worker(
                rx,
                output,
                family,
                scale,
                cell_w,
                cell_h,
                baseline,
                opts.frame_style,
                layout,
            )
        })
        .expect("failed to spawn gif stream worker");

    Ok(GifStreamHandle { tx, join })
}

pub fn render_gif(rec: &Recording, opts: &RenderOptions, out: &Path) -> Result<()> {
    let stream = spawn_gif_stream(
        GifStreamConfig {
            cols: rec.cols,
            rows: rec.rows,
        },
        RenderOptions {
            font_path: opts.font_path.clone(),
            font_size: opts.font_size,
            frame_style: rec.frame_style,
        },
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
    let family = loaded.family;
    let scale = PxScale::from(opts.font_size);
    let scaled = family.regular.as_scaled(scale);
    let cell_w = scaled
        .h_advance(family.regular.glyph_id('M'))
        .ceil()
        .max(1.0) as u32;
    let cell_h = (scaled.height() + scaled.line_gap()).ceil().max(1.0) as u32;
    let baseline = scaled.ascent().ceil() as u32;
    let mut warned_missing_faces = HashSet::new();
    let buf = rasterize_raw_frame(
        frame,
        &family,
        scale,
        cell_w,
        cell_h,
        baseline,
        opts.frame_style,
        &mut warned_missing_faces,
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
    family: FontFamily,
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

    let mut warned_missing_faces: HashSet<&'static str> = HashSet::new();
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
            &family,
            scale,
            cell_w,
            cell_h,
            baseline,
            frame_style,
            &mut warned_missing_faces,
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
    family: &FontFamily,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    frame_style: FrameStyle,
    warned_missing_faces: &mut HashSet<&'static str>,
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

            let primary_font =
                select_primary_font_for_cell(family, cell.flags, warned_missing_faces);
            let mut pen_x = x as f32;
            for ch in cell.text.chars() {
                let font = select_font_for_char(primary_font, &family.fallback_regular, ch);
                let glyph_id = font.glyph_id(ch);
                let glyph: Glyph = glyph_id.with_scale(scale);
                if let Some(outline) = font.outline_glyph(glyph) {
                    let bounds = outline.px_bounds();
                    outline.draw(|gx, gy, coverage| {
                        let px = pen_x as i32 + bounds.min.x as i32 + gx as i32;
                        let py = y as i32 + baseline as i32 + bounds.min.y as i32 + gy as i32;
                        if px < 0 || py < 0 {
                            return;
                        }
                        let (px, py) = (px as u32, py as u32);
                        if px >= layout.canvas_w || py >= layout.canvas_h {
                            return;
                        }
                        blend_pixel(&mut buf, layout.canvas_w, px, py, fg, coverage);
                    });
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
    let frame_w = cols as u32 * cell_w + frame_style.padding_px * 2;
    let frame_h = rows as u32 * cell_h + frame_style.padding_px * 2 + bar_h;
    LayoutMetrics {
        canvas_w: frame_w + frame_style.margin_px * 2,
        canvas_h: frame_h + frame_style.margin_px * 2,
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

/// Load the requested font family. If `path` is provided we use that file as
/// the regular face only. Otherwise we load embedded JetBrains Mono faces.
#[derive(Debug)]
struct LoadedFontFamily {
    family: FontFamily,
    description: String,
}

fn load_font_family(path: Option<&str>) -> Result<LoadedFontFamily> {
    if let Some(p) = path {
        let bytes = std::fs::read(p).with_context(|| format!("reading font {p}"))?;
        let regular = FontArc::try_from_vec(bytes).context("invalid font file")?;
        return Ok(LoadedFontFamily {
            family: FontFamily {
                regular,
                bold: None,
                italic: None,
                bold_italic: None,
                fallback_regular: Vec::new(),
            },
            description: format!("explicit path: {p}"),
        });
    }

    let (fallback_regular, fallback_names) = load_default_fallback_faces();

    // Deterministic default for GIF rendering: use embedded JetBrains Mono
    // Nerd Font (Mono variant),
    // compressed as WOFF2 at build time and decompressed at runtime.
    // License text is shipped in `licenses/JETBRAINSMONO-OFL-1.1.txt`.
    Ok(LoadedFontFamily {
        family: FontFamily {
            regular: decode_embedded_face(
                "JetBrainsMonoNerdFontMono-Regular.woff2",
                EMBEDDED_JETBRAINS_NERD_MONO_REGULAR_WOFF2,
            )?,
            bold: try_embedded_face(
                "JetBrainsMonoNerdFontMono-Bold.woff2",
                EMBEDDED_JETBRAINS_NERD_MONO_BOLD_WOFF2,
            ),
            italic: try_embedded_face(
                "JetBrainsMonoNerdFontMono-Italic.woff2",
                EMBEDDED_JETBRAINS_NERD_MONO_ITALIC_WOFF2,
            ),
            bold_italic: try_embedded_face(
                "JetBrainsMonoNerdFontMono-BoldItalic.woff2",
                EMBEDDED_JETBRAINS_NERD_MONO_BOLD_ITALIC_WOFF2,
            ),
            fallback_regular,
        },
        description: if fallback_names.is_empty() {
            "embedded default: JetBrainsMono Nerd Font Mono family".to_string()
        } else {
            format!(
                "embedded default: JetBrainsMono Nerd Font Mono family + fallbacks [{}]",
                fallback_names.join(" -> ")
            )
        },
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

fn select_primary_font_for_cell<'a>(
    family: &'a FontFamily,
    flags: u8,
    warned_missing_faces: &mut HashSet<&'static str>,
) -> &'a FontArc {
    let want_bold = flags & style_flags::BOLD != 0;
    let want_italic = flags & style_flags::ITALIC != 0;

    if want_bold && want_italic {
        if let Some(font) = &family.bold_italic {
            return font;
        }
        warn_missing_once(
            warned_missing_faces,
            "bold-italic",
            "falling back to regular",
        );
        return &family.regular;
    }

    if want_bold {
        if let Some(font) = &family.bold {
            return font;
        }
        warn_missing_once(warned_missing_faces, "bold", "falling back to regular");
        return &family.regular;
    }

    if want_italic {
        if let Some(font) = &family.italic {
            return font;
        }
        warn_missing_once(warned_missing_faces, "italic", "falling back to regular");
        return &family.regular;
    }

    &family.regular
}

fn select_font_for_char<'a>(
    primary: &'a FontArc,
    fallback: &'a [FontArc],
    ch: char,
) -> &'a FontArc {
    if has_glyph(primary, ch) {
        return primary;
    }

    for font in fallback {
        if has_glyph(font, ch) {
            return font;
        }
    }

    primary
}

fn has_glyph(font: &FontArc, ch: char) -> bool {
    font.glyph_id(ch).0 != 0
}

fn warn_missing_once(
    warned_missing_faces: &mut HashSet<&'static str>,
    style: &'static str,
    fallback: &'static str,
) {
    if warned_missing_faces.insert(style) {
        warn!(style, fallback, "requested font style face is unavailable");
    }
}
