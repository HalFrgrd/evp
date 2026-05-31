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
//! We use a **cell-based** animation model. Instead of showing/hiding
//! entire frame groups (which causes characters to flash briefly then
//! disappear), we track each cell position across frames and emit one
//! SVG element per "span" — the time interval during which a cell has
//! identical visual content (text, fg, bg, style). Each element uses a
//! `<set>` to be visible only for its span duration.
//!
//! A single dummy `<animate id="t">` provides a synchronised global
//! timer of `total_duration` seconds with `repeatCount="indefinite"`.
//! Each cell-span element references this timer so the animation loops.
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

use crate::render_common::is_box_drawing;
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};

use std::collections::HashSet;
use base64::prelude::*;
use subsetter::{subset, GlyphRemapper};
use ttf_parser::Face;
use woff2_patched::convert_woff2_to_ttf;

use crate::{
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, ViewportConfig},
    style::{rgb_hex, window_bar_dot_metrics},
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

fn subset_font(woff2_data: &[u8], chars: &HashSet<char>) -> Option<Vec<u8>> {
    let ttf = convert_woff2_to_ttf(&mut std::io::Cursor::new(woff2_data)).ok()?;
    let face = Face::parse(&ttf, 0).ok()?;

    let mut glyphs = Vec::new();
    glyphs.push(0); // .notdef
    for c in chars {
        if let Some(glyph_id) = face.glyph_index(*c) {
            glyphs.push(glyph_id.0);
        }
    }

    glyphs.sort_unstable();
    glyphs.dedup();

    let mapper = GlyphRemapper::new_from_glyphs_sorted(&glyphs);
    subset(&ttf, 0, &mapper).ok()
}

fn generate_style_block(frames: &[RawFrame]) -> String {
    let mut used_chars = HashSet::new();
    for frame in frames {
        for cell in &frame.cells {
            for c in cell.text.chars() {
                used_chars.insert(c);
            }
        }
    }

    let subset = |data: &[u8]| -> String {
        match subset_font(data, &used_chars) {
            Some(subset_data) => format!("url(data:font/ttf;base64,{}) format('truetype')", BASE64_STANDARD.encode(&subset_data)),
            None => format!("url(data:font/woff2;base64,{}) format('woff2')", BASE64_STANDARD.encode(data)),
        }
    };

    format!(
        r#"<style>
@font-face {{ font-family: 'JetBrainsMono Nerd Font Mono'; src: {jb_reg}; font-weight: normal; font-style: normal; }}
@font-face {{ font-family: 'JetBrainsMono Nerd Font Mono'; src: {jb_bold}; font-weight: bold; font-style: normal; }}
@font-face {{ font-family: 'JetBrainsMono Nerd Font Mono'; src: {jb_ital}; font-weight: normal; font-style: italic; }}
@font-face {{ font-family: 'JetBrainsMono Nerd Font Mono'; src: {jb_bold_ital}; font-weight: bold; font-style: italic; }}
@font-face {{ font-family: 'Noto Sans Mono'; src: {ns_mono}; }}
@font-face {{ font-family: 'Noto Sans Symbols 2'; src: {ns_sym2}; }}
@font-face {{ font-family: 'Noto Sans Mono CJK JP'; src: {ns_cjk}; }}
@font-face {{ font-family: 'unifont_upper'; src: {uni_upper}; }}
@font-face {{ font-family: 'unifont_csur'; src: {uni_csur}; }}
</style>
"#,
        jb_reg = subset(EMBEDDED_JETBRAINS_NERD_MONO_REGULAR_WOFF2),
        jb_bold = subset(EMBEDDED_JETBRAINS_NERD_MONO_BOLD_WOFF2),
        jb_ital = subset(EMBEDDED_JETBRAINS_NERD_MONO_ITALIC_WOFF2),
        jb_bold_ital = subset(EMBEDDED_JETBRAINS_NERD_MONO_BOLD_ITALIC_WOFF2),
        ns_mono = subset(EMBEDDED_NOTO_SANS_MONO_REGULAR_WOFF2),
        ns_sym2 = subset(EMBEDDED_NOTO_SANS_SYMBOLS2_REGULAR_WOFF2),
        ns_cjk = subset(EMBEDDED_NOTO_SANS_MONO_CJK_JP_SUBSET_WOFF2),
        uni_upper = subset(EMBEDDED_UNIFONT_UPPER_WOFF2),
        uni_csur = subset(EMBEDDED_UNIFONT_CSUR_WOFF2),
    )
}

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
    cfg: ViewportConfig,
    opts: SvgOptions,
    output: PathBuf,
) -> Result<SvgStreamHandle> {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RAW_FRAME_CONSUMER_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-svg-stream".into())
        .spawn(move || run_svg_stream_worker(rx, cfg, opts, output))
        .expect("failed to spawn svg stream worker");
    Ok(SvgStreamHandle { tx, join })
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            font_family: "'JetBrainsMono Nerd Font Mono', 'Noto Sans Mono', 'Noto Sans Symbols 2', 'Noto Sans Mono CJK JP', 'unifont_upper', 'unifont_csur', ui-monospace, Menlo, Consolas, 'DejaVu Sans Mono', monospace".to_string(),
            font_size: 16.0,
        }
    }
}

/// Render `rec` as an animated SVG written to `out`.
pub fn render_svg(rec: &Recording, opts: &SvgOptions, out: &Path) -> Result<()> {
    let stream = spawn_svg_stream(
        ViewportConfig::new(
            rec.cols,
            rec.rows,
            rec.framerate,
            rec.cell_width_px,
            rec.cell_height_px,
            rec.frame_style,
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

/// Same as [`render_svg`] but returns the document as a `String` —
/// useful for tests and for callers embedding the SVG inline.
pub fn render_svg_to_string(rec: &Recording, opts: &SvgOptions) -> Result<String> {
    let cfg = ViewportConfig::new(
        rec.cols,
        rec.rows,
        rec.framerate,
        rec.cell_width_px,
        rec.cell_height_px,
        rec.frame_style,
    );

    // Reconstruct every frame up-front.
    let mut frames: Vec<RawFrame> = Vec::with_capacity(rec.frames.len());
    for i in 0..rec.frames.len() {
        let f = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        frames.push(f);
    }

    Ok(render_from_frames(&frames, cfg, opts))
}

// ---------------------------------------------------------------------------
// Core rendering logic shared by both paths
// ---------------------------------------------------------------------------

/// Visual state of a single cell, used for diffing across frames.
#[derive(Clone, PartialEq, Eq)]
struct CellVisual {
    text: String,
    fg: [u8; 3],
    bg: [u8; 3],
    flags: u8,
}

impl CellVisual {
    fn from_snap(cell: &crate::recording::CellSnap) -> Self {
        let (fg, bg) = effective_colors(cell);
        Self {
            text: cell.text.clone(),
            fg,
            bg,
            flags: cell.flags,
        }
    }

    fn is_blank(&self, default_bg: [u8; 3]) -> bool {
        self.text.is_empty() && self.bg == default_bg
    }
}

/// A time span during which a cell has a particular visual state.
struct CellSpan {
    row: u16,
    col: u16,
    start_ms: u32,
    end_ms: u32,
    visual: CellVisual,
    default_bg: [u8; 3],
}

/// A time span during which the cursor is at a particular position.
struct CursorSpan {
    col: u16,
    row: u16,
    start_ms: u32,
    end_ms: u32,
    color: [u8; 3],
}

fn render_from_frames(frames: &[RawFrame], cfg: ViewportConfig, opts: &SvgOptions) -> String {
    let canvas_w = cfg.canvas_w;
    let canvas_h = cfg.canvas_h;

    if frames.is_empty() {
        return format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">
</svg>
"#,
            w = canvas_w,
            h = canvas_h,
        );
    }

    // Total animation duration.
    let last_t_ms = frames.last().map(|f| f.t_ms).unwrap_or(0);
    let frame_ms = if cfg.framerate > 0 {
        1000 / cfg.framerate.max(1)
    } else {
        33
    };
    let total_ms = (last_t_ms + frame_ms).max(1);
    let total_s = total_ms as f32 / 1000.0;

    let cols = frames[0].cols;
    let rows = frames[0].rows;
    let num_cells = cols as usize * rows as usize;

    // Build cell spans: track when each cell changes across frames.
    let mut cell_spans: Vec<CellSpan> = Vec::new();
    // Current state per cell: (visual, start_ms, default_bg at start)
    let mut current: Vec<(CellVisual, u32, [u8; 3])> = Vec::with_capacity(num_cells);

    // Initialize with first frame.
    let first = &frames[0];
    for cell in &first.cells {
        current.push((CellVisual::from_snap(cell), first.t_ms, first.default_bg));
    }

    // Process subsequent frames.
    for frame in frames.iter().skip(1) {
        for idx in 0..num_cells {
            let new_visual = CellVisual::from_snap(&frame.cells[idx]);
            let (ref old_visual, start_ms, old_default_bg) = current[idx];
            // If visual changed, or default_bg changed (which affects "blank" rendering)
            if *old_visual != new_visual || (old_visual.bg == old_default_bg && new_visual.bg == frame.default_bg && old_default_bg != frame.default_bg) {
                // Flush the old span if it's not visually blank
                if !old_visual.is_blank(old_default_bg) {
                    let row = (idx / cols as usize) as u16;
                    let col = (idx % cols as usize) as u16;
                    cell_spans.push(CellSpan {
                        row,
                        col,
                        start_ms,
                        end_ms: frame.t_ms,
                        visual: old_visual.clone(),
                        default_bg: old_default_bg,
                    });
                }
                current[idx] = (new_visual, frame.t_ms, frame.default_bg);
            }
        }
    }

    // Flush remaining spans.
    for idx in 0..num_cells {
        let (ref visual, start_ms, default_bg) = current[idx];
        if !visual.is_blank(default_bg) {
            let row = (idx / cols as usize) as u16;
            let col = (idx % cols as usize) as u16;
            cell_spans.push(CellSpan {
                row,
                col,
                start_ms,
                end_ms: total_ms,
                visual: visual.clone(),
                default_bg,
            });
        }
    }

    // Build cursor spans.
    let mut cursor_spans: Vec<CursorSpan> = Vec::new();
    let mut cur_cursor: Option<(u16, u16, u32, [u8; 3])> = None; // (col, row, start_ms, color)

    for frame in frames.iter() {
        match (cur_cursor, frame.cursor) {
            (None, Some((cx, cy))) => {
                cur_cursor = Some((cx, cy, frame.t_ms, frame.default_fg));
            }
            (Some((ocx, ocy, start, color)), Some((cx, cy))) => {
                if ocx != cx || ocy != cy || color != frame.default_fg {
                    cursor_spans.push(CursorSpan {
                        col: ocx,
                        row: ocy,
                        start_ms: start,
                        end_ms: frame.t_ms,
                        color,
                    });
                    cur_cursor = Some((cx, cy, frame.t_ms, frame.default_fg));
                }
            }
            (Some((ocx, ocy, start, color)), None) => {
                cursor_spans.push(CursorSpan {
                    col: ocx,
                    row: ocy,
                    start_ms: start,
                    end_ms: frame.t_ms,
                    color,
                });
                cur_cursor = None;
            }
            (None, None) => {}
        }
    }
    // Flush remaining cursor.
    if let Some((cx, cy, start, color)) = cur_cursor {
        cursor_spans.push(CursorSpan {
            col: cx,
            row: cy,
            start_ms: start,
            end_ms: total_ms,
            color,
        });
    }

    // Now emit SVG.
    let mut s = String::with_capacity(64 * 1024);
    s.push_str(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="{font}" font-size="{fs}" xml:space="preserve">
{style}"#,
        w = canvas_w,
        h = canvas_h,
        font = escape_attr(&opts.font_family),
        fs = opts.font_size,
        style = generate_style_block(frames)
    ));

    // Canvas background (static).
    s.push_str(&format!(
        r#"<rect width="{w}" height="{h}" fill="{bg}"/>
"#,
        w = canvas_w,
        h = canvas_h,
        bg = rgb_hex(cfg.frame_style.margin_fill),
    ));

    // Clip path for rounded corners.
    if cfg.frame_style.border_radius_px > 0 {
        s.push_str(&format!(
            r#"<defs><clipPath id="frame-clip"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" ry="{r}"/></clipPath></defs>"#,
            x = cfg.frame_x,
            y = cfg.frame_y,
            w = cfg.frame_w,
            h = cfg.frame_h,
            r = cfg.frame_style.border_radius_px.min(cfg.frame_w / 2).min(cfg.frame_h / 2),
        ));
    }

    // Frame background (static).
    let clip_attr = if cfg.frame_style.border_radius_px > 0 {
        r#" clip-path="url(#frame-clip)""#
    } else {
        ""
    };
    s.push_str(&format!(
        r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{bg}"{clip}/>
"#,
        x = cfg.frame_x,
        y = cfg.frame_y,
        w = cfg.frame_w,
        h = cfg.frame_h,
        bg = rgb_hex(frames[0].default_bg),
        clip = clip_attr,
    ));

    // Window bar (static).
    if cfg.frame_style.window_bar.enabled() {
        emit_window_bar(&mut s, cfg);
    }

    // Master timer.
    s.push_str(&format!(
        r#"<rect width="0" height="0"><animate id="t" attributeName="x" from="0" to="0" dur="{dur}s" repeatCount="indefinite"/></rect>
"#,
        dur = total_s
    ));

    let cell_w = cfg.cell_width_px.max(1);
    let cell_h = cfg.cell_height_px.max(1);
    let baseline = (opts.font_size * 0.8).round() as u32;

    // Determine if all spans cover the full duration (static content optimization).
    let is_static = |span_start: u32, span_end: u32| -> bool {
        span_start == 0 && span_end >= total_ms
    };

    // Emit cell spans grouped by time window for efficiency.
    // First: background rects.
    for span in &cell_spans {
        if span.visual.bg == span.default_bg {
            continue;
        }
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h;
        if is_static(span.start_ms, span.end_ms) {
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{cw}" height="{ch}" fill="{f}"{clip}/>"#,
                cw = cell_w,
                ch = cell_h,
                f = rgb_hex(span.visual.bg),
                clip = clip_attr,
            ));
        } else {
            let begin_s = span.start_ms as f32 / 1000.0;
            let end_s = span.end_ms as f32 / 1000.0;
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{cw}" height="{ch}" fill="{f}"{clip} visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b}s" end="t.begin+{e}s"/></rect>"#,
                cw = cell_w,
                ch = cell_h,
                f = rgb_hex(span.visual.bg),
                clip = clip_attr,
                b = begin_s,
                e = end_s,
            ));
        }
    }

    // Text spans.
    for span in &cell_spans {
        if span.visual.text.is_empty() {
            continue;
        }
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h + baseline;
        let weight = if span.visual.flags & style_flags::BOLD != 0 {
            " font-weight=\"bold\""
        } else {
            ""
        };
        let italic = if span.visual.flags & style_flags::ITALIC != 0 {
            " font-style=\"italic\""
        } else {
            ""
        };
        let decoration =
            if span.visual.flags & style_flags::UNDERLINE != 0 && span.visual.flags & style_flags::STRIKETHROUGH != 0 {
                " text-decoration=\"underline line-through\""
            } else if span.visual.flags & style_flags::UNDERLINE != 0 {
                " text-decoration=\"underline\""
            } else if span.visual.flags & style_flags::STRIKETHROUGH != 0 {
                " text-decoration=\"line-through\""
            } else {
                ""
            };

        let is_box = span.visual.text.chars().any(is_box_drawing);

        let text_elem = if is_box {
            let text_length = cell_w;
            let bbox_h = opts.font_size * 0.8;
            let scale_y = (cell_h as f32 / bbox_h).max(1.0);
            let transform = if scale_y > 1.0 {
                let cell_center_y = (y as f32 - baseline as f32) + (cell_h as f32 / 2.0);
                let char_center_y = y as f32 - (opts.font_size * 0.3);
                format!(
                    r#" transform="translate(0, {cy}) scale(1, {scale_y}) translate(0, -{char_center_y})""#,
                    cy = cell_center_y,
                    char_center_y = char_center_y,
                    scale_y = scale_y
                )
            } else {
                String::new()
            };
            format!(
                r#"<text x="{x}" y="{y}" fill="{fg}"{w}{i}{d}{transform} textLength="{text_length}" lengthAdjust="spacingAndGlyphs">{txt}</text>"#,
                fg = rgb_hex(span.visual.fg),
                w = weight,
                i = italic,
                d = decoration,
                transform = transform,
                text_length = text_length,
                txt = escape_text(&span.visual.text),
            )
        } else {
            format!(
                r#"<text x="{x}" y="{y}" fill="{fg}"{w}{i}{d}>{txt}</text>"#,
                fg = rgb_hex(span.visual.fg),
                w = weight,
                i = italic,
                d = decoration,
                txt = escape_text(&span.visual.text),
            )
        };

        if is_static(span.start_ms, span.end_ms) {
            s.push_str(&text_elem);
        } else {
            let begin_s = span.start_ms as f32 / 1000.0;
            let end_s = span.end_ms as f32 / 1000.0;
            s.push_str(&format!(
                r#"<g visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b}s" end="t.begin+{e}s"/>{elem}</g>"#,
                b = begin_s,
                e = end_s,
                elem = text_elem,
            ));
        }
    }

    // Cursor spans.
    for span in &cursor_spans {
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h;
        if is_static(span.start_ms, span.end_ms) {
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{cw}" height="{ch}" fill="{c}" fill-opacity="0.7"/>"#,
                cw = cell_w,
                ch = cell_h,
                c = rgb_hex(span.color),
            ));
        } else {
            let begin_s = span.start_ms as f32 / 1000.0;
            let end_s = span.end_ms as f32 / 1000.0;
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{cw}" height="{ch}" fill="{c}" fill-opacity="0.7" visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b}s" end="t.begin+{e}s"/></rect>"#,
                cw = cell_w,
                ch = cell_h,
                c = rgb_hex(span.color),
                b = begin_s,
                e = end_s,
            ));
        }
    }

    s.push_str("\n</svg>\n");
    s
}

fn run_svg_stream_worker(
    rx: Receiver<RawFrame>,
    cfg: ViewportConfig,
    opts: SvgOptions,
    out: PathBuf,
) -> Result<()> {
    let mut frames: Vec<RawFrame> = Vec::new();
    while let Ok(frame) = rx.recv() {
        frames.push(frame);
    }

    let s = render_from_frames(&frames, cfg, &opts);

    let mut file = File::create(&out).with_context(|| format!("create {}", out.display()))?;
    file.write_all(s.as_bytes())
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn effective_colors(cell: &crate::recording::CellSnap) -> ([u8; 3], [u8; 3]) {
    let (mut fg, bg) = if cell.flags & style_flags::INVERSE != 0 {
        (cell.bg, cell.fg)
    } else {
        (cell.fg, cell.bg)
    };
    // SGR 2 dim: blend fg 50% toward bg (equivalent to opacity 0.5).
    if cell.flags & style_flags::DIM != 0 {
        fg = dim_color(fg, bg);
    }
    (fg, bg)
}

/// SGR 2 dim: blend foreground 50% toward background (opacity 0.5 equivalent).
fn dim_color(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    [
        ((fg[0] as u16 + bg[0] as u16) / 2) as u8,
        ((fg[1] as u16 + bg[1] as u16) / 2) as u8,
        ((fg[2] as u16 + bg[2] as u16) / 2) as u8,
    ]
}

fn emit_window_bar(s: &mut String, cfg: ViewportConfig) {
    let style = cfg.frame_style.window_bar;
    let (radius, gap) = window_bar_dot_metrics(cfg.bar_h);
    let dots_w = radius * 2 * 3 + gap * 2;
    let start_x = if style.align_right() {
        cfg.frame_x + cfg.frame_w.saturating_sub(dots_w + gap)
    } else {
        cfg.frame_x + gap
    };
    let cy = cfg.frame_y + cfg.bar_h / 2;
    for (idx, color) in [[255, 95, 86], [255, 189, 46], [39, 201, 63]]
        .iter()
        .enumerate()
    {
        let cx = start_x + idx as u32 * (radius * 2 + gap) + radius;
        if style.outlined() {
            s.push_str(&format!(
                r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{stroke}" stroke-width="2"/>"#,
                r = radius,
                stroke = rgb_hex(*color),
            ));
        } else {
            s.push_str(&format!(
                r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"/>"#,
                r = radius,
                fill = rgb_hex(*color),
            ));
        }
    }
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
    use crate::{
        FrameStyle,
        recording::{CellSnap, Frame},
    };

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
            frame_style: FrameStyle {
                padding_px: 4,
                ..FrameStyle::default()
            },
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
    }

    #[test]
    fn explicit_canvas_size_controls_svg_dimensions() {
        let mut rec = synth_recording();
        rec.frame_style.canvas_width_px = Some(1200);
        rec.frame_style.canvas_height_px = Some(600);
        let svg = render_svg_to_string(&rec, &SvgOptions::default()).unwrap();
        assert!(svg.contains(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 600" width="1200" height="600""#
        ));
        assert!(svg.contains(r#"<rect width="1200" height="600""#));
    }

    #[test]
    fn escapes_xml_special_chars() {
        assert_eq!(escape_text("<&>"), "&lt;&amp;&gt;");
        assert_eq!(escape_attr("\"<&>'"), "&quot;&lt;&amp;&gt;&apos;");
    }
}
