//! Render a [`Recording`] to an animated GIF.
//!
//! We rasterise each frame as an RGB buffer using `ab_glyph`, then quantise
//! per‑frame with `color_quant::NeuQuant` and write GIF frames via the
//! `gif` crate. Diff frames are reconstructed by [`Recording::reconstruct`]
//! before being drawn.

use std::{fs::File, path::Path};

use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use color_quant::NeuQuant;
use gif::{Encoder, Frame, Repeat};
use tracing::info;

use crate::recording::{RawFrame, Recording, style_flags};

const EMBEDDED_IOSEVKA_TERM_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/SGr-IosevkaTerm-Regular.ttc");

pub struct RenderOptions {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub padding_px: u32,
}

pub fn render_gif(rec: &Recording, opts: &RenderOptions, out: &Path) -> Result<()> {
    let loaded = load_font(opts.font_path.as_deref())?;
    info!(font = %loaded.description, "using font for gif rendering");

    let font_data = loaded.bytes;
    let font = FontArc::try_from_vec(font_data).context("invalid font file")?;
    let scale = PxScale::from(opts.font_size);
    let scaled = font.as_scaled(scale);

    // Measure cell size from a representative monospace glyph.
    let cell_w = scaled.h_advance(font.glyph_id('M')).ceil().max(1.0) as u32;
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

    for i in 0..rec.frames.len() {
        let frame = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;

        let buf = rasterize_frame(
            &frame,
            &font,
            scale,
            cell_w,
            cell_h,
            baseline,
            opts.padding_px,
        );

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

        let mut gif_frame = Frame::from_rgb_speed(canvas_w as u16, canvas_h as u16, &buf, 10);
        gif_frame.delay = delay_cs;
        encoder.write_frame(&gif_frame)?;
        prev_buf = Some(buf);
    }
    Ok(())
}

fn rasterize_frame(
    frame: &RawFrame,
    font: &FontArc,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    padding: u32,
) -> Vec<u8> {
    let canvas_w = frame.cols as u32 * cell_w + padding * 2;
    let canvas_h = frame.rows as u32 * cell_h + padding * 2;
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

    let scaled = font.as_scaled(scale);
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

            // Draw cell background if it differs from the canvas default.
            if bg != frame.default_bg || cell.flags & style_flags::INVERSE != 0 {
                fill_rect(&mut buf, canvas_w, x, y, cell_w, cell_h, bg);
            }

            if cell.text.is_empty() {
                continue;
            }

            // Draw each character of the cell. Most cells contain a single
            // grapheme but combining marks may produce more.
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

            // Fake-bold: draw again shifted by 1px.
            if cell.flags & style_flags::BOLD != 0 {
                let mut pen_x = x as f32 + 1.0;
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

    // We currently emit `from_rgb_speed`-friendly raw RGB; the gif crate
    // performs quantisation internally. NeuQuant is referenced in the docs
    // for users who want to build a custom palette pipeline later.
    let _ = NeuQuant::new;
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

/// Load the requested font file as bytes. If `path` is provided we use it
/// directly, otherwise we ask `fontdb` for any monospace family installed
/// on the system. Returns an error if no usable font is found – there is
/// no embedded fallback in `evp` (the user can pass `--font /path/to.ttf`).
#[derive(Debug)]
struct LoadedFont {
    bytes: Vec<u8>,
    description: String,
}

fn load_font(path: Option<&str>) -> Result<LoadedFont> {
    if let Some(p) = path {
        return Ok(LoadedFont {
            bytes: std::fs::read(p).with_context(|| format!("reading font {p}"))?,
            description: format!("explicit path: {p}"),
        });
    }

    // Deterministic default for GIF rendering: use embedded Iosevka Term.
    // License text is shipped in `licenses/IOSEVKA-OFL-1.1.txt`.
    Ok(LoadedFont {
        bytes: EMBEDDED_IOSEVKA_TERM_REGULAR.to_vec(),
        description: "embedded default: SGr-IosevkaTerm-Regular.ttc".to_string(),
    })
}

#[allow(dead_code)]
fn _system_monospace_fallback() -> Result<LoadedFont> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let query = fontdb::Query {
        families: &[fontdb::Family::Monospace],
        ..Default::default()
    };
    let id = db
        .query(&query)
        .ok_or_else(|| anyhow!("no monospace font found on the system"))?;
    let face = db.face(id).ok_or_else(|| anyhow!("font face not found"))?;
    match &face.source {
        fontdb::Source::File(path) => Ok(LoadedFont {
            bytes: std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?,
            description: format!("system monospace file: {}", path.display()),
        }),
        fontdb::Source::SharedFile(path, data) => Ok(LoadedFont {
            bytes: data.as_ref().as_ref().to_vec(),
            description: format!("system monospace shared file: {}", path.display()),
        }),
        fontdb::Source::Binary(data) => Ok(LoadedFont {
            bytes: data.as_ref().as_ref().to_vec(),
            description: "system monospace binary source".to_string(),
        }),
    }
}
