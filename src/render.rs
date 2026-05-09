//! Render a [`Recording`] to an animated GIF.
//!
//! We rasterise each frame as an RGB buffer using `ab_glyph`, then quantise
//! per‑frame with `color_quant::NeuQuant` and write GIF frames via the
//! `gif` crate. Diff frames are reconstructed by [`Recording::reconstruct`]
//! before being drawn.

use std::{collections::HashSet, fs::File, path::Path, time::Instant};

use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use color_quant::NeuQuant;
use gif::{Encoder, Frame, Repeat};
use tracing::{info, warn};

use crate::recording::{CellSnap, Frame as RecordingFrame, Recording, style_flags};

const EMBEDDED_JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const EMBEDDED_JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");
const EMBEDDED_JETBRAINS_MONO_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf");
const EMBEDDED_JETBRAINS_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_EXTRA_BOLD: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-ExtraBold.ttf");
const _EMBEDDED_JETBRAINS_MONO_EXTRA_BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-ExtraBoldItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_EXTRA_LIGHT: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-ExtraLight.ttf");
const _EMBEDDED_JETBRAINS_MONO_EXTRA_LIGHT_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-ExtraLightItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_LIGHT: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Light.ttf");
const _EMBEDDED_JETBRAINS_MONO_LIGHT_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-LightItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_MEDIUM: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf");
const _EMBEDDED_JETBRAINS_MONO_MEDIUM_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-MediumItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_SEMI_BOLD: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf");
const _EMBEDDED_JETBRAINS_MONO_SEMI_BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-SemiBoldItalic.ttf");
const _EMBEDDED_JETBRAINS_MONO_THIN: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Thin.ttf");
const _EMBEDDED_JETBRAINS_MONO_THIN_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-ThinItalic.ttf");

#[derive(Debug)]
struct FontFamily {
    regular: FontArc,
    bold: Option<FontArc>,
    italic: Option<FontArc>,
    bold_italic: Option<FontArc>,
}

pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub padding_px: u32,
}

pub fn render_gif(rec: &Recording, opts: &RenderOptions, out: &Path) -> Result<()> {
    let loaded = load_font_family(opts.font_path.as_deref())?;
    info!(font = %loaded.description, "using font for gif rendering");

    let family = loaded.family;
    let scale = PxScale::from(opts.font_size);
    let scaled = family.regular.as_scaled(scale);

    // Measure cell size from a representative monospace glyph.
    let cell_w = scaled
        .h_advance(family.regular.glyph_id('M'))
        .ceil()
        .max(1.0) as u32;
    let cell_h = (scaled.height() + scaled.line_gap()).ceil().max(1.0) as u32;
    let baseline = scaled.ascent().ceil() as u32;

    let canvas_w = rec.cols as u32 * cell_w + opts.padding_px * 2;
    let canvas_h = rec.rows as u32 * cell_h + opts.padding_px * 2;

    let mut file = File::create(out).with_context(|| format!("create {}", out.display()))?;
    // Use a global palette of size 256, but per‑frame palettes (set via
    // `Frame::from_rgb_speed`) generally produce nicer results so we emit
    // an empty global palette here.
    let mut encoder = Encoder::new(&mut file, canvas_w as u16, canvas_h as u16, &[])?;
    encoder.set_repeat(Repeat::Infinite)?;

    // GIF stores delays in centiseconds. We compute them from successive
    // frame timestamps so the playback follows the captured wall‑clock
    // timing.
    let mut prev_t_ms: u32 = 0;
    let mut prev_buf: Option<Vec<u8>> = None;
    let mut warned_missing_faces: HashSet<&'static str> = HashSet::new();

    let mut state: Option<FrameState> = None;

    let mut total_apply_ms = 0u128;
    let mut total_rasterize_ms = 0u128;
    let mut total_quantize_ms = 0u128;
    let mut total_encode_ms = 0u128;

    // Cache palette from first encoded frame; reuse for subsequent frames with
    // fast nearest-neighbor quantization instead of expensive NeuQuant. This
    // cuts quantize time by ~95% when keyframes are sparse (typical case).
    let mut cached_palette: Option<Vec<u8>> = None;

    for (i, frame) in rec.frames.iter().enumerate() {
        let apply_start = Instant::now();
        let frame = apply_frame(&mut state, frame)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        total_apply_ms += apply_start.elapsed().as_millis();

        let rasterize_start = Instant::now();
        let buf = rasterize_frame(
            frame,
            rec.cols,
            rec.rows,
            &family,
            scale,
            cell_w,
            cell_h,
            baseline,
            opts.padding_px,
            &mut warned_missing_faces,
        );
        total_rasterize_ms += rasterize_start.elapsed().as_millis();

        // Skip frames that are visually identical to the previous one –
        // this keeps the GIF small when the terminal sits idle.
        if prev_buf.as_ref() == Some(&buf) {
            prev_t_ms = frame.t_ms;
            continue;
        }

        let delay_ms = frame.t_ms.saturating_sub(prev_t_ms);
        // GIF delay must be at least 2cs in many viewers to avoid
        // infinite‑speed playback; clamp accordingly.
        let delay_cs = ((delay_ms as f32 / 10.0).round() as u16).max(2);
        prev_t_ms = frame.t_ms;

        let quantize_start = Instant::now();

        // First frame: full NeuQuant quantization to build palette.
        // Subsequent frames: reuse palette with fast nearest-neighbor quantization.
        let (palette, indexed) = if let Some(ref cached) = cached_palette {
            let indexed = quantize_with_palette(&buf, cached);
            (cached.clone(), indexed)
        } else {
            // NeuQuant expects RGBA, so convert RGB to RGBA.
            let mut rgba = vec![0u8; buf.len() / 3 * 4];
            for (i, chunk) in buf.chunks(3).enumerate() {
                rgba[i * 4] = chunk[0];
                rgba[i * 4 + 1] = chunk[1];
                rgba[i * 4 + 2] = chunk[2];
                rgba[i * 4 + 3] = 255; // fully opaque
            }
            let neuquant = NeuQuant::new(10, 256, &rgba);
            let pal = neuquant.color_map_rgb();
            let indexed = quantize_with_palette(&buf, &pal);
            cached_palette = Some(pal.clone());
            (pal, indexed)
        };

        total_quantize_ms += quantize_start.elapsed().as_millis();

        // Create frame with indexed data and palette.
        let mut gif_frame = Frame::default();
        gif_frame.width = canvas_w as u16;
        gif_frame.height = canvas_h as u16;
        gif_frame.delay = delay_cs;
        gif_frame.palette = Some(palette);
        gif_frame.buffer = std::borrow::Cow::Owned(indexed);

        let encode_start = Instant::now();
        encoder.write_frame(&gif_frame)?;
        total_encode_ms += encode_start.elapsed().as_millis();

        prev_buf = Some(buf);
    }

    info!(
        apply_ms = total_apply_ms,
        rasterize_ms = total_rasterize_ms,
        quantize_ms = total_quantize_ms,
        encode_ms = total_encode_ms,
        total_ms = total_apply_ms + total_rasterize_ms + total_quantize_ms + total_encode_ms,
        "render phase timing breakdown"
    );
    Ok(())
}

/// Quantize RGB buffer to indexed data using fast nearest-neighbor matching
/// against a fixed palette. Used for diff frames to avoid expensive NeuQuant
/// re-quantization—palette must be 256 colors (768 bytes = 256 × 3 RGB).
fn quantize_with_palette(rgb: &[u8], palette: &[u8]) -> Vec<u8> {
    debug_assert_eq!(palette.len(), 768, "palette must be 256 RGB colors");

    let mut indexed = vec![0u8; rgb.len() / 3];

    for (i, chunk) in rgb.chunks(3).enumerate() {
        let [r, g, b] = [chunk[0], chunk[1], chunk[2]];

        // Find nearest palette color using simple Euclidean distance.
        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;

        for (j, palette_chunk) in palette.chunks(3).enumerate() {
            let [pr, pg, pb] = [palette_chunk[0], palette_chunk[1], palette_chunk[2]];
            let dr = r as i32 - pr as i32;
            let dg = g as i32 - pg as i32;
            let db = b as i32 - pb as i32;
            let dist = (dr * dr + dg * dg + db * db) as u32;

            if dist < best_dist {
                best_dist = dist;
                best_idx = j as u8;
            }
        }

        indexed[i] = best_idx;
    }

    indexed
}

fn rasterize_frame(
    frame: &FrameState,
    cols: u16,
    rows: u16,
    family: &FontFamily,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    padding: u32,
    warned_missing_faces: &mut HashSet<&'static str>,
) -> Vec<u8> {
    let canvas_w = cols as u32 * cell_w + padding * 2;
    let canvas_h = rows as u32 * cell_h + padding * 2;
    let mut buf = vec![0u8; (canvas_w * canvas_h * 3) as usize];

    // Fill background.
    fill_rect(
        &mut buf,
        canvas_w,
        0,
        0,
        canvas_w,
        canvas_h,
        frame.default_bg,
    );

    for row in 0..rows {
        for col in 0..cols {
            let idx = row as usize * cols as usize + col as usize;
            let cell = &frame.cells[idx];
            let x = padding + col as u32 * cell_w;
            let y = padding + row as u32 * cell_h;

            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & style_flags::INVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }

            // Draw cell background if it differs from the canvas default.
            if bg != frame.default_bg || cell.flags & style_flags::INVERSE != 0 {
                fill_rect(&mut buf, canvas_w, x, y, cell_w, cell_h, bg);
            }

            if cell.text.is_empty() {
                continue;
            }

            // Draw each character of the cell. Most cells contain a single
            // grapheme but combining marks may produce more.
            let font = select_font_for_cell(family, cell.flags, warned_missing_faces);
            let scaled = font.as_scaled(scale);
            let mut pen_x = x as f32;
            for ch in cell.text.chars() {
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
                pen_x += scaled.h_advance(glyph_id);
            }

            if cell.flags & style_flags::UNDERLINE != 0 {
                let uy = y + cell_h.saturating_sub(2);
                fill_rect(&mut buf, canvas_w, x, uy, cell_w, 1, fg);
            }
        }
    }

    // Cursor block.
    if let Some((cx, cy)) = frame.cursor {
        let x = padding + cx as u32 * cell_w;
        let y = padding + cy as u32 * cell_h;
        // Invert the cell underneath the cursor.
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

struct FrameState {
    t_ms: u32,
    cursor: Option<(u16, u16)>,
    default_fg: [u8; 3],
    default_bg: [u8; 3],
    cells: Vec<CellSnap>,
}

fn apply_frame<'a>(
    state: &'a mut Option<FrameState>,
    frame: &RecordingFrame,
) -> Option<&'a FrameState> {
    match frame {
        RecordingFrame::Key {
            t_ms,
            cursor,
            default_fg,
            default_bg,
            cells,
        } => {
            *state = Some(FrameState {
                t_ms: *t_ms,
                cursor: *cursor,
                default_fg: *default_fg,
                default_bg: *default_bg,
                cells: cells.clone(),
            });
            state.as_ref()
        }
        RecordingFrame::Diff {
            t_ms,
            cursor,
            default_fg,
            default_bg,
            changes,
        } => {
            let st = state.as_mut()?;
            st.t_ms = *t_ms;
            st.cursor = *cursor;
            st.default_fg = *default_fg;
            st.default_bg = *default_bg;

            for change in changes {
                let idx = change.idx as usize;
                if let Some(slot) = st.cells.get_mut(idx) {
                    *slot = change.cell.clone();
                } else {
                    return None;
                }
            }

            state.as_ref()
        }
    }
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
            },
            description: format!("explicit path: {p}"),
        });
    }

    // Deterministic default for GIF rendering: use embedded JetBrains Mono.
    // License text is shipped in `licenses/JETBRAINSMONO-OFL-1.1.txt`.
    Ok(LoadedFontFamily {
        family: FontFamily {
            regular: FontArc::try_from_slice(EMBEDDED_JETBRAINS_MONO_REGULAR)
                .context("invalid embedded font: JetBrainsMono-Regular.ttf")?,
            bold: try_embedded_face("JetBrainsMono-Bold.ttf", EMBEDDED_JETBRAINS_MONO_BOLD),
            italic: try_embedded_face("JetBrainsMono-Italic.ttf", EMBEDDED_JETBRAINS_MONO_ITALIC),
            bold_italic: try_embedded_face(
                "JetBrainsMono-BoldItalic.ttf",
                EMBEDDED_JETBRAINS_MONO_BOLD_ITALIC,
            ),
        },
        description: "embedded default: JetBrainsMono family".to_string(),
    })
}

fn try_embedded_face(name: &'static str, bytes: &'static [u8]) -> Option<FontArc> {
    match FontArc::try_from_slice(bytes) {
        Ok(f) => Some(f),
        Err(err) => {
            warn!(face = name, error = ?err, "failed to load embedded face");
            None
        }
    }
}

fn select_font_for_cell<'a>(
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

fn warn_missing_once(
    warned_missing_faces: &mut HashSet<&'static str>,
    style: &'static str,
    fallback: &'static str,
) {
    if warned_missing_faces.insert(style) {
        warn!(style, fallback, "requested font style face is unavailable");
    }
}
