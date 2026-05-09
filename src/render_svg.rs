//! Animated SVG renderer for [`Recording`].
//!
//! ## Why SVG?
//!
//! - Vector text: the rendered glyphs stay sharp at any zoom level and
//!   are selectable / searchable in the browser.
//! - Tiny diff-friendly artifact: identical-to-previous frames are
//!   skipped entirely; per-frame data is rectangles + text runs rather
//!   than rasterised pixels.
//! - Plays in any browser (and on github.com when embedded as `<img>`)
//!   via SMIL animations — no JS required.
//!
//! ## Animation model
//!
//! We emit one `<g>` per unique frame, all hidden by default via
//! `visibility="hidden"`. A single dummy `<animate id="t">` provides a
//! synchronised global timer of `total_duration` seconds with
//! `repeatCount="indefinite"`. Each frame group then has a `<set>` that
//! flips its visibility on at `t.begin + tN` and back off at
//! `t.begin + tN+1`. When the master timer wraps, every set re-fires, so
//! the animation loops forever.
//!
//! ## Style mapping
//!
//! - Cell background  → `<rect fill="#rrggbb">` (only emitted when not
//!   the canvas default).
//! - Cell text        → `<text>`; runs of cells with identical
//!   foreground/style are coalesced into a single `<text>` element.
//! - Bold / italic    → `font-weight` / `font-style` attributes.
//! - Underline        → a 1-pixel `<rect>` at the cell baseline.
//! - Inverse          → the cell's bg/fg are swapped before emission.
//! - Cursor           → an inverted-fill rect over the cell, sourced from
//!   the recorded cursor position.

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::recording::{RawFrame, Recording, style_flags};

const SVG_STREAM_CHANNEL_CAPACITY: usize = 4096;

/// Tunables for the SVG renderer.
#[derive(Debug, Clone)]
pub struct SvgOptions {
    /// CSS `font-family` value applied to every `<text>` element.
    /// Defaults to a stack of common monospace families.
    pub font_family: String,
    /// `font-size` (CSS px) for the rendered glyphs. The recording's
    /// `cell_width_px` / `cell_height_px` are *layout* metrics — we
    /// honour them as cell sizes regardless, but `font_size` is what
    /// actually controls glyph height in the browser.
    pub font_size: f32,
}

pub struct SvgStreamConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub padding_px: u32,
}

pub struct SvgStreamHandle {
    pub tx: Sender<RawFrame>,
    join: JoinHandle<Result<()>>,
}

impl SvgStreamHandle {
    pub fn join(self) -> Result<()> {
        drop(self.tx);
        self.join.join().expect("svg stream worker panicked")
    }
}

pub fn spawn_svg_stream(
    cfg: SvgStreamConfig,
    opts: SvgOptions,
    output: PathBuf,
) -> Result<SvgStreamHandle> {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) = bounded(SVG_STREAM_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-svg-stream".into())
        .spawn(move || run_svg_stream_worker(rx, cfg, opts, output))
        .expect("failed to spawn svg stream worker");
    Ok(SvgStreamHandle { tx, join })
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            font_family: "ui-monospace, Menlo, Consolas, 'DejaVu Sans Mono', monospace".to_string(),
            font_size: 16.0,
        }
    }
}

/// Render `rec` as an animated SVG written to `out`.
pub fn render_svg(rec: &Recording, opts: &SvgOptions, out: &Path) -> Result<()> {
    let stream = spawn_svg_stream(
        SvgStreamConfig {
            cols: rec.cols,
            rows: rec.rows,
            framerate: rec.framerate,
            cell_width_px: rec.cell_width_px,
            cell_height_px: rec.cell_height_px,
            padding_px: rec.padding_px,
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

/// Same as [`render_svg`] but returns the document as a `String` —
/// useful for tests and for callers embedding the SVG inline.
pub fn render_svg_to_string(rec: &Recording, opts: &SvgOptions) -> Result<String> {
    let cell_w = rec.cell_width_px.max(1);
    let cell_h = rec.cell_height_px.max(1);
    let pad = rec.padding_px;
    let canvas_w = rec.cols as u32 * cell_w + pad * 2;
    let canvas_h = rec.rows as u32 * cell_h + pad * 2;

    // Total animation duration, in seconds, derived from the last frame's
    // timestamp plus a single frame interval so the final frame is held
    // for at least one tick before the loop restarts.
    let last_t_ms = rec.frames.last().map(|f| f.t_ms()).unwrap_or(0);
    let frame_ms = if rec.framerate > 0 {
        1000 / rec.framerate.max(1)
    } else {
        33
    };
    let total_ms = (last_t_ms + frame_ms).max(1);
    let total_s = total_ms as f32 / 1000.0;

    let default_bg_hex = match rec.frames.first() {
        Some(crate::recording::Frame::Key { default_bg, .. }) => *default_bg,
        _ => [0, 0, 0],
    };

    // Reconstruct every frame up-front so we can emit them in document
    // order and skip duplicates trivially. For the recording sizes we
    // care about (a few hundred frames at most) this is fine.
    let mut frames: Vec<RawFrame> = Vec::with_capacity(rec.frames.len());
    for i in 0..rec.frames.len() {
        let f = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        frames.push(f);
    }

    // Compute (start_ms, end_ms) windows for each unique frame. Frames
    // identical to their predecessor extend the previous window rather
    // than emitting their own group.
    let mut windows: Vec<Window> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let same_as_prev = windows
            .last()
            .is_some_and(|w| frames_visually_identical(&frames[w.frame_idx], f));
        if same_as_prev {
            // extend previous window to this frame's start; the actual
            // end is set by the *next* frame, or by total_ms.
            continue;
        }
        if let Some(prev) = windows.last_mut() {
            prev.end_ms = f.t_ms;
        }
        windows.push(Window {
            start_ms: f.t_ms,
            end_ms: total_ms,
            frame_idx: i,
        });
    }

    let mut s = String::with_capacity(64 * 1024);
    s.push_str(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="{font}" font-size="{fs}" xml:space="preserve">
"#,
        w = canvas_w,
        h = canvas_h,
        font = escape_attr(&opts.font_family),
        fs = opts.font_size,
    ));

    // Canvas background.
    s.push_str(&format!(
        r#"<rect width="{w}" height="{h}" fill="{bg}"/>
"#,
        w = canvas_w,
        h = canvas_h,
        bg = rgb_hex(default_bg_hex),
    ));

    // Master timer. We animate a no-op attribute on a zero-size rect so
    // we can reference its `begin` from each frame's <set>.
    s.push_str(&format!(
        r#"<rect width="0" height="0"><animate id="t" attributeName="x" from="0" to="0" dur="{dur}s" repeatCount="indefinite"/></rect>
"#,
        dur = total_s
    ));

    // Each frame group.
    for w in &windows {
        let frame = &frames[w.frame_idx];
        let begin_s = w.start_ms as f32 / 1000.0;
        let end_s = w.end_ms as f32 / 1000.0;
        s.push_str(&format!(
            r#"<g visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b}s" end="t.begin+{e}s"/>"#,
            b = begin_s,
            e = end_s,
        ));
        emit_frame_body(&mut s, frame, cell_w, cell_h, pad, opts.font_size);
        s.push_str("</g>\n");
    }

    s.push_str("</svg>\n");
    Ok(s)
}

struct Window {
    start_ms: u32,
    end_ms: u32,
    frame_idx: usize,
}

fn run_svg_stream_worker(
    rx: Receiver<RawFrame>,
    cfg: SvgStreamConfig,
    opts: SvgOptions,
    out: PathBuf,
) -> Result<()> {
    let mut frames: Vec<RawFrame> = Vec::new();
    let mut windows: Vec<Window> = Vec::new();
    let mut last_t_ms = 0u32;

    while let Ok(frame) = rx.recv() {
        last_t_ms = frame.t_ms;
        let same_as_prev = windows
            .last()
            .is_some_and(|w| frames_visually_identical(&frames[w.frame_idx], &frame));
        if same_as_prev {
            if let Some(prev) = windows.last_mut() {
                prev.end_ms = frame.t_ms;
            }
            continue;
        }

        if let Some(prev) = windows.last_mut() {
            prev.end_ms = frame.t_ms;
        }

        let idx = frames.len();
        frames.push(frame.clone());
        windows.push(Window {
            start_ms: frame.t_ms,
            end_ms: frame.t_ms,
            frame_idx: idx,
        });
    }

    let frame_ms = if cfg.framerate > 0 {
        1000 / cfg.framerate.max(1)
    } else {
        33
    };
    let total_ms = (last_t_ms + frame_ms).max(1);
    if let Some(last) = windows.last_mut() {
        last.end_ms = total_ms;
    }

    let default_bg_hex = frames.first().map(|f| f.default_bg).unwrap_or([0, 0, 0]);
    let canvas_w = cfg.cols as u32 * cfg.cell_width_px.max(1) + cfg.padding_px * 2;
    let canvas_h = cfg.rows as u32 * cfg.cell_height_px.max(1) + cfg.padding_px * 2;
    let total_s = total_ms as f32 / 1000.0;

    let mut s = String::with_capacity(64 * 1024);
    s.push_str(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="{font}" font-size="{fs}" xml:space="preserve">
"#,
        w = canvas_w,
        h = canvas_h,
        font = escape_attr(&opts.font_family),
        fs = opts.font_size,
    ));
    s.push_str(&format!(
        r#"<rect width="{w}" height="{h}" fill="{bg}"/>
"#,
        w = canvas_w,
        h = canvas_h,
        bg = rgb_hex(default_bg_hex),
    ));
    s.push_str(&format!(
        r#"<rect width="0" height="0"><animate id="t" attributeName="x" from="0" to="0" dur="{dur}s" repeatCount="indefinite"/></rect>
"#,
        dur = total_s
    ));

    for w in &windows {
        let frame = &frames[w.frame_idx];
        let begin_s = w.start_ms as f32 / 1000.0;
        let end_s = w.end_ms as f32 / 1000.0;
        s.push_str(&format!(
            r#"<g visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b}s" end="t.begin+{e}s"/>"#,
            b = begin_s,
            e = end_s,
        ));
        emit_frame_body(
            &mut s,
            frame,
            cfg.cell_width_px.max(1),
            cfg.cell_height_px.max(1),
            cfg.padding_px,
            opts.font_size,
        );
        s.push_str("</g>\n");
    }

    s.push_str("</svg>\n");

    let mut file = File::create(&out).with_context(|| format!("create {}", out.display()))?;
    file.write_all(s.as_bytes())
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-frame emission
// ---------------------------------------------------------------------------

fn emit_frame_body(
    s: &mut String,
    frame: &RawFrame,
    cell_w: u32,
    cell_h: u32,
    pad: u32,
    font_size: f32,
) {
    // Background rectangles: collapse runs of identical bg in the same row.
    for row in 0..frame.rows {
        let mut col = 0u16;
        while col < frame.cols {
            let cell = &frame.cells[row as usize * frame.cols as usize + col as usize];
            let (_fg, bg) = effective_colors(cell);
            if bg == frame.default_bg {
                col += 1;
                continue;
            }
            let mut run_end = col + 1;
            while run_end < frame.cols {
                let next = &frame.cells[row as usize * frame.cols as usize + run_end as usize];
                let (_, nbg) = effective_colors(next);
                if nbg != bg {
                    break;
                }
                run_end += 1;
            }
            let x = pad + col as u32 * cell_w;
            let y = pad + row as u32 * cell_h;
            let rw = (run_end - col) as u32 * cell_w;
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{rw}" height="{ch}" fill="{f}"/>"#,
                ch = cell_h,
                f = rgb_hex(bg),
            ));
            col = run_end;
        }
    }

    // Text runs: collapse runs of identical (fg, style) cells with
    // non-empty text in the same row.
    let baseline = (font_size * 0.8).round() as u32; // approximation
    for row in 0..frame.rows {
        let mut col = 0u16;
        while col < frame.cols {
            let cell = &frame.cells[row as usize * frame.cols as usize + col as usize];
            if cell.text.is_empty() {
                col += 1;
                continue;
            }
            let (fg, _) = effective_colors(cell);
            let style = cell.flags;
            let mut run = String::new();
            run.push_str(&cell.text);
            let mut run_end = col + 1;
            while run_end < frame.cols {
                let next = &frame.cells[row as usize * frame.cols as usize + run_end as usize];
                let (nfg, _) = effective_colors(next);
                if next.text.is_empty() || nfg != fg || next.flags != style {
                    break;
                }
                run.push_str(&next.text);
                run_end += 1;
            }
            let x = pad + col as u32 * cell_w;
            let y = pad + row as u32 * cell_h + baseline;
            let weight = if style & style_flags::BOLD != 0 {
                " font-weight=\"bold\""
            } else {
                ""
            };
            let italic = if style & style_flags::ITALIC != 0 {
                " font-style=\"italic\""
            } else {
                ""
            };
            let decoration =
                if style & style_flags::UNDERLINE != 0 && style & style_flags::STRIKETHROUGH != 0 {
                    " text-decoration=\"underline line-through\""
                } else if style & style_flags::UNDERLINE != 0 {
                    " text-decoration=\"underline\""
                } else if style & style_flags::STRIKETHROUGH != 0 {
                    " text-decoration=\"line-through\""
                } else {
                    ""
                };
            s.push_str(&format!(
                r#"<text x="{x}" y="{y}" fill="{fg}"{w}{i}{d}>{txt}</text>"#,
                fg = rgb_hex(fg),
                w = weight,
                i = italic,
                d = decoration,
                txt = escape_text(&run),
            ));
            col = run_end;
        }
    }

    // Cursor: draw an inverted block over the cell.
    if let Some((cx, cy)) = frame.cursor {
        let x = pad + cx as u32 * cell_w;
        let y = pad + cy as u32 * cell_h;
        s.push_str(&format!(
            r#"<rect x="{x}" y="{y}" width="{cw}" height="{ch}" fill="{c}" fill-opacity="0.7"/>"#,
            cw = cell_w,
            ch = cell_h,
            c = rgb_hex(frame.default_fg),
        ));
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn effective_colors(cell: &crate::recording::CellSnap) -> ([u8; 3], [u8; 3]) {
    if cell.flags & style_flags::INVERSE != 0 {
        (cell.bg, cell.fg)
    } else {
        (cell.fg, cell.bg)
    }
}

fn frames_visually_identical(a: &RawFrame, b: &RawFrame) -> bool {
    a.cells == b.cells && a.cursor == b.cursor && a.default_bg == b.default_bg
}

fn rgb_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Escape a string for use as an XML attribute value.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Escape a string for use as XML text content. Spaces are kept verbatim
/// because we set `xml:space="preserve"` on the root `<svg>`.
fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{CellSnap, Frame};

    fn synth_recording() -> Recording {
        let blank = CellSnap::blank([255, 255, 255], [0, 0, 0]);
        let mut cells = vec![blank.clone(); 8];
        cells[0] = CellSnap {
            text: "h".into(),
            fg: [255, 255, 255],
            bg: [0, 0, 0],
            flags: 0,
        };
        cells[1] = CellSnap {
            text: "i".into(),
            fg: [255, 255, 255],
            bg: [0, 0, 0],
            flags: 0,
        };
        Recording {
            cols: 4,
            rows: 2,
            framerate: 10,
            cell_width_px: 8,
            cell_height_px: 16,
            padding_px: 4,
            frames: vec![Frame::Key {
                t_ms: 0,
                cursor: Some((2, 0)),
                default_fg: [255, 255, 255],
                default_bg: [0, 0, 0],
                cells,
            }],
        }
    }

    #[test]
    fn renders_well_formed_svg() {
        let rec = synth_recording();
        let svg = render_svg_to_string(&rec, &SvgOptions::default()).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(svg.ends_with("</svg>\n"));
        // Text content present.
        assert!(svg.contains(">h<") || svg.contains(">hi<"));
        // Master timer.
        assert!(svg.contains(r#"id="t""#));
        // Frame group with visibility set.
        assert!(svg.contains("visibility=\"hidden\""));
        assert!(svg.contains("attributeName=\"visibility\""));
    }

    #[test]
    fn escapes_xml_special_chars() {
        assert_eq!(escape_text("<&>"), "&lt;&amp;&gt;");
        assert_eq!(escape_attr("\"<&>'"), "&quot;&lt;&amp;&gt;&apos;");
    }
}
