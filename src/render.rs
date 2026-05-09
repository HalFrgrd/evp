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
use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use gifski::{Settings, progress};
use tracing::{info, warn};
use woofwoof::decompress;

use crate::recording::{RawFrame, Recording, style_flags};

// Rendering can briefly lag behind capture on busy systems; this queue absorbs
// bursts so the upstream pipeline usually stays lock-free.
const RENDER_STREAM_CHANNEL_CAPACITY: usize = 4096;

const EMBEDDED_JETBRAINS_NERD_MONO_REGULAR_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMonoNerdFontMono-Regular.woff2"));
const EMBEDDED_JETBRAINS_NERD_MONO_BOLD_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMonoNerdFontMono-Bold.woff2"));
const EMBEDDED_JETBRAINS_NERD_MONO_ITALIC_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMonoNerdFontMono-Italic.woff2"));
const EMBEDDED_JETBRAINS_NERD_MONO_BOLD_ITALIC_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMonoNerdFontMono-BoldItalic.woff2"));
const EMBEDDED_UNIFONT_UPPER_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/unifont_upper-17.0.04.woff2"));
const EMBEDDED_UNIFONT_CSUR_WOFF2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/unifont_csur-17.0.04.woff2"));

#[derive(Debug)]
struct FontFamily {
    regular: FontArc,
    bold: Option<FontArc>,
    italic: Option<FontArc>,
    bold_italic: Option<FontArc>,
    fallback_regular: Vec<FontArc>,
}

const DEFAULT_SYSTEM_FALLBACK_FONTS: [(&str, Weight); 2] = [
    ("NotoSansJP-Medium", Weight::MEDIUM),
    ("Noto Sans JP", Weight::MEDIUM),
];

pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub padding_px: u32,
}

pub struct GifStreamConfig {
    pub cols: u16,
    pub rows: u16,
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
    let canvas_w = cfg.cols as u32 * cell_w + opts.padding_px * 2;
    let canvas_h = cfg.rows as u32 * cell_h + opts.padding_px * 2;

    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RENDER_STREAM_CHANNEL_CAPACITY);
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
                opts.padding_px,
                canvas_w,
                canvas_h,
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
            padding_px: opts.padding_px,
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
    padding: u32,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<()> {
    let (collector, writer) = gifski::new(Settings {
        width: Some(canvas_w),
        height: Some(canvas_h),
        quality: 100,
        fast: false,
        repeat: gifski::Repeat::Infinite,
    })
    .context("initialize gifski encoder")?;

    let mut warned_missing_faces: HashSet<&'static str> = HashSet::new();
    let mut prev_t_ms = 0u32;
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
            padding,
            &mut warned_missing_faces,
        );

        if prev_buf.as_ref() == Some(&buf) {
            prev_t_ms = frame.t_ms;
            continue;
        }

        let delay_ms = frame.t_ms.saturating_sub(prev_t_ms);
        let delay_cs = ((delay_ms as f32 / 10.0).round() as u16).max(2);
        prev_t_ms = frame.t_ms;

        let rgba = rgb_to_rgba(&buf);
        let frame_img = imgref::ImgVec::new(rgba, canvas_w as usize, canvas_h as usize);
        collector
            .add_frame_rgba(frame_index, frame_img, delay_cs as f64 / 100.0)
            .context("add frame to gifski")?;

        frame_index += 1;
        prev_buf = Some(buf);
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
    padding: u32,
    warned_missing_faces: &mut HashSet<&'static str>,
) -> Vec<u8> {
    let canvas_w = frame.cols as u32 * cell_w + padding * 2;
    let canvas_h = frame.rows as u32 * cell_h + padding * 2;
    let mut buf = vec![0u8; (canvas_w * canvas_h * 3) as usize];

    fill_rect(
        &mut buf,
        canvas_w,
        0,
        0,
        canvas_w,
        canvas_h,
        frame.default_bg,
    );

    for row in 0..frame.rows {
        for col in 0..frame.cols {
            let idx = row as usize * frame.cols as usize + col as usize;
            let cell = &frame.cells[idx];
            let x = padding + col as u32 * cell_w;
            let y = padding + row as u32 * cell_h;

            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & style_flags::INVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }

            if bg != frame.default_bg || cell.flags & style_flags::INVERSE != 0 {
                fill_rect(&mut buf, canvas_w, x, y, cell_w, cell_h, bg);
            }

            if cell.text.is_empty() {
                continue;
            }

            let primary_font = select_primary_font_for_cell(family, cell.flags, warned_missing_faces);
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
                        if px >= canvas_w || py >= canvas_h {
                            return;
                        }
                        blend_pixel(&mut buf, canvas_w, px, py, fg, coverage);
                    });
                }
                let scaled = font.as_scaled(scale);
                pen_x += scaled.h_advance(glyph_id);
            }

            if cell.flags & style_flags::UNDERLINE != 0 {
                let uy = y + cell_h.saturating_sub(2);
                fill_rect(&mut buf, canvas_w, x, uy, cell_w, 1, fg);
            }
        }
    }

    if let Some((cx, cy)) = frame.cursor {
        let x = padding + cx as u32 * cell_w;
        let y = padding + cy as u32 * cell_h;
        invert_rect(&mut buf, canvas_w, x, y, cell_w, cell_h);
    }

    buf
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
    let mut db = Database::new();
    db.load_system_fonts();

    let mut faces = Vec::new();
    let mut names = Vec::new();

    // 1) System NotoSansJP-Medium (or Noto Sans JP).
    for (family_name, weight) in DEFAULT_SYSTEM_FALLBACK_FONTS {
        let query = Query {
            families: &[Family::Name(family_name)],
            weight,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };

        if let Some(id) = db.query(&query)
            && let Some(bytes) = db.with_face_data(id, |data, _idx| data.to_vec())
        {
            match FontArc::try_from_vec(bytes) {
                Ok(font) => {
                    faces.push(font);
                    names.push(family_name.to_string());
                }
                Err(err) => {
                    warn!(family = family_name, error = ?err, "failed to load fallback font face");
                }
            }
        } else {
            warn!(family = family_name, "fallback font not found on system");
        }
    }

    // 2) Embedded unifont_upper (U+10000 and above coverage).
    match decode_embedded_face("unifont_upper-17.0.04.woff2", EMBEDDED_UNIFONT_UPPER_WOFF2) {
        Ok(font) => {
            faces.push(font);
            names.push("unifont_upper-17.0.04 (embedded)".to_string());
        }
        Err(err) => {
            warn!(error = ?err, "failed to load embedded fallback font face");
        }
    }

    // 3) Embedded unifont_csur (CSUR/PUA coverage).
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
    let ttf = decompress(bytes)
        .with_context(|| format!("failed to decompress embedded WOFF2 face: {name}"))?;
    FontArc::try_from_vec(ttf).with_context(|| format!("invalid embedded font face: {name}"))
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

fn select_font_for_char<'a>(primary: &'a FontArc, fallback: &'a [FontArc], ch: char) -> &'a FontArc {
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
