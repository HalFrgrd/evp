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

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::render_common::is_box_drawing;
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::font::load_font_family;
use base64::prelude::*;
use std::collections::{BTreeSet, HashMap};

use crate::{
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, ViewportConfig},
    style::{rgb_hex, window_bar_dot_metrics},
};

fn generate_style_block(frames: &[RawFrame], opts: &SvgOptions) -> Result<String> {
    if !opts.embed_fonts {
        return Ok(String::new());
    }

    let mut style = String::new();
    style.push_str("<style>\n");

    let loaded = load_font_family(opts.font_path.as_deref())?;
    let mut used_fonts: HashMap<usize, BTreeSet<char>> = HashMap::new();

    // Check which fonts are actually selected/used by cells.
    for frame in frames {
        for cell in &frame.cells {
            for c in cell.text.chars() {
                let (idx, _) = loaded.font_set.select_for_char(cell.flags, c);
                used_fonts.entry(idx).or_default().insert(c);
            }
        }
    }

    if used_fonts.is_empty() {
        return Ok(String::new());
    }

    // Embed the used fonts.
    let mut sorted_indices: Vec<_> = used_fonts.keys().copied().collect();
    sorted_indices.sort();

    for idx in sorted_indices {
        if idx >= loaded.font_set.fonts.len() {
            continue;
        }
        let info = &loaded.font_set.fonts[idx];
        let chars = &used_fonts[&idx];
        if chars.is_empty() {
            continue;
        }

        let (font_bytes, format_str, mime_type) = match info.subset(chars) {
            Ok(subset) => (subset, "woff2", "font/woff2"),
            Err(err) => {
                tracing::warn!(
                    "failed to subset font '{}' ({} chars), embedding the entire font: {:?}",
                    info.family_name,
                    chars.len(),
                    err
                );
                if let Some(ref woff2) = info.woff2_bytes {
                    (woff2.clone(), "woff2", "font/woff2")
                } else {
                    let is_otf = info.ttf_bytes.starts_with(b"OTTO");
                    let (fmt, mime) = if is_otf {
                        ("opentype", "font/opentype")
                    } else {
                        ("truetype", "font/truetype")
                    };
                    (info.ttf_bytes.clone(), fmt, mime)
                }
            }
        };

        let encoded = BASE64_STANDARD.encode(font_bytes);
        let src = format!("url(data:{mime_type};base64,{encoded})");

        let css_template = format!(
            "@font-face {{ font-family: '{}'; src: {} format('{}'); font-weight: {}; font-style: {}; }}\n",
            info.family_name, src, format_str, info.weight, info.style
        );
        style.push_str(&css_template);
    }

    style.push_str("</style>\n");
    Ok(style)
}

/// Tunables for the SVG renderer.
#[derive(Debug, Clone)]
pub struct SvgOptions {
    /// Optional path to a custom TTF font file.
    pub font_path: Option<String>,
    /// CSS `font-family` value applied to every `<text>` element.
    /// Defaults to a stack of common monospace families.
    pub font_family: String,
    /// `font-size` (CSS px) for the rendered glyphs. The recording's
    /// `cell_width_px` / `cell_height_px` are *layout* metrics — we
    /// honour them as cell sizes regardless, but `font_size` is what
    /// actually controls glyph height in the browser.
    pub font_size: f32,
    /// Whether to embed base64-encoded subset font data in the SVG.
    /// If false, relies entirely on system fonts.
    pub embed_fonts: bool,
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
            font_path: None,
            font_family: "'JetBrainsMono Nerd Font Mono', 'Noto Sans Mono', 'Noto Sans Symbols 2', 'Noto Sans Mono CJK JP', 'unifont_upper', 'unifont_csur', ui-monospace, Menlo, Consolas, 'DejaVu Sans Mono', monospace".to_string(),
            font_size: 16.0,
            embed_fonts: true,
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
        rec.font_size_px,
        rec.char_height_px,
        rec.ascent_px,
        rec.letter_spacing,
    );

    // Reconstruct every frame up-front.
    let mut frames: Vec<RawFrame> = Vec::with_capacity(rec.frames.len());
    for i in 0..rec.frames.len() {
        let f = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        frames.push(f);
    }

    render_from_frames(&frames, cfg, opts)
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

#[derive(Clone, Debug, PartialEq)]
pub struct TSpan {
    pub x_coords: Vec<f32>,
    pub text: String,
    pub fg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub is_box: bool,
    pub scale_y: f32,
    pub cell_center_y_offset: f32,
    pub char_center_y_offset: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub baseline: u32,
    pub letter_spacing: f32,
}

impl TSpan {
    fn to_svg_string(&self, color_classes: &HashMap<[u8; 3], String>) -> String {
        let x_str = self
            .x_coords
            .iter()
            .map(|x| format!("{:.2}", x))
            .collect::<Vec<_>>()
            .join(" ");
        let weight = if self.bold {
            " font-weight=\"bold\""
        } else {
            ""
        };
        let italic = if self.italic {
            " font-style=\"italic\""
        } else {
            ""
        };
        let decoration = if self.underline && self.strikethrough {
            " text-decoration=\"underline line-through\""
        } else if self.underline {
            " text-decoration=\"underline\""
        } else if self.strikethrough {
            " text-decoration=\"line-through\""
        } else {
            ""
        };
        let style = if self.letter_spacing != 0.0 {
            format!(r#" style="letter-spacing: {:.2}px;""#, self.letter_spacing)
        } else {
            String::new()
        };

        let fill_attr = if let Some(cls) = color_classes.get(&self.fg) {
            format!(r#" class="{}""#, cls)
        } else {
            format!(r#" fill="{}""#, rgb_hex(self.fg))
        };

        format!(
            r#"<tspan x="{x}"{fill}{w}{i}{d}{s}>{txt}</tspan>"#,
            x = x_str,
            fill = fill_attr,
            w = weight,
            i = italic,
            d = decoration,
            s = style,
            txt = escape_text(&self.text),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct YAnimation {
    pub begin_ms: u32,
    pub segments: Vec<(u32, u32)>, // (y_value, start_ms_of_segment)
    pub dur_ms: u32,
}

impl YAnimation {
    fn to_svg_string(&self) -> String {
        let values_str = self
            .segments
            .iter()
            .map(|(y, _)| y.to_string())
            .collect::<Vec<_>>()
            .join(";");
        let key_times_str = self
            .segments
            .iter()
            .map(|(_, start)| format!("{:.6}", (start - self.begin_ms) as f32 / self.dur_ms as f32))
            .collect::<Vec<_>>()
            .join(";");
        let dur_s = self.dur_ms as f32 / 1000.0;
        let begin_s = self.begin_ms as f32 / 1000.0;
        format!(
            r#"<animate attributeName="y" calcMode="discrete" values="{values}" keyTimes="{key_times}" dur="{dur:.2}s" begin="t.begin+{begin:.2}s" fill="freeze"/>"#,
            values = values_str,
            key_times = key_times_str,
            dur = dur_s,
            begin = begin_s,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextElement {
    pub y: u32,
    pub y_animation: Option<YAnimation>,
    pub start_ms: u32,
    pub end_ms: u32,
    pub tspans: Vec<TSpan>,
}

impl TextElement {
    pub fn content_equals(&self, other: &Self) -> bool {
        if self.tspans.len() != other.tspans.len() {
            return false;
        }
        for (a, b) in self.tspans.iter().zip(other.tspans.iter()) {
            if a.x_coords != b.x_coords
                || a.text != b.text
                || a.fg != b.fg
                || a.bold != b.bold
                || a.italic != b.italic
                || a.underline != b.underline
                || a.strikethrough != b.strikethrough
                || a.is_box != b.is_box
                || a.scale_y != b.scale_y
                || a.letter_spacing != b.letter_spacing
            {
                return false;
            }
        }
        true
    }

    pub fn to_svg_string(&self, color_classes: &HashMap<[u8; 3], String>, total_ms: u32) -> String {
        let mut text_content = String::new();
        let mut current_non_box: Vec<&TSpan> = Vec::new();

        let flush_non_box = |non_box: &mut Vec<&TSpan>, s: &mut String| {
            if non_box.is_empty() {
                return;
            }
            s.push_str(&format!(r#"<text y="{}">"#, self.y));
            if let Some(ref anim) = self.y_animation {
                s.push_str(&anim.to_svg_string());
            }
            for tspan in non_box.iter() {
                s.push_str(&tspan.to_svg_string(color_classes));
            }
            s.push_str("</text>");
            non_box.clear();
        };

        for tspan in &self.tspans {
            if tspan.is_box {
                flush_non_box(&mut current_non_box, &mut text_content);

                let text_length = tspan.cell_w;
                let scale_y = tspan.scale_y;
                let y = self.y;
                let transform = if scale_y > 1.0 {
                    let cy = y as f32 + tspan.cell_center_y_offset;
                    let char_center_y = y as f32 + tspan.char_center_y_offset;
                    format!(
                        r#" transform="translate(0, {cy}) scale(1, {scale_y}) translate(0, -{char_center_y})""#,
                        cy = cy,
                        char_center_y = char_center_y,
                        scale_y = scale_y
                    )
                } else {
                    String::new()
                };

                text_content.push_str(&format!(
                    r#"<text x="{x}" y="{y}"{transform} textLength="{text_length}" lengthAdjust="spacingAndGlyphs">"#,
                    x = tspan.x_coords[0],
                    y = y,
                    transform = transform,
                    text_length = text_length,
                ));
                if let Some(ref anim) = self.y_animation {
                    text_content.push_str(&anim.to_svg_string());
                }
                text_content.push_str(&tspan.to_svg_string(color_classes));
                text_content.push_str("</text>");
            } else {
                current_non_box.push(tspan);
            }
        }
        flush_non_box(&mut current_non_box, &mut text_content);

        if is_static(self.start_ms, self.end_ms, total_ms) {
            text_content
        } else {
            let begin_s = self.start_ms as f32 / 1000.0;
            let end_s = self.end_ms as f32 / 1000.0;
            format!(
                r#"<g visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b:.2}s" end="t.begin+{e:.2}s"/>{elem}</g>"#,
                b = begin_s,
                e = end_s,
                elem = text_content,
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BgRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub fill: [u8; 3],
    pub start_ms: u32,
    pub end_ms: u32,
    pub clip_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub fill: [u8; 3],
    pub start_ms: u32,
    pub end_ms: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarCircle {
    pub cx: u32,
    pub cy: u32,
    pub r: u32,
    pub fill: Option<[u8; 3]>,
    pub stroke: Option<[u8; 3]>,
}

impl WindowBarCircle {
    fn to_svg_string(&self) -> String {
        if let Some(fill) = self.fill {
            format!(
                r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"/>"#,
                cx = self.cx,
                cy = self.cy,
                r = self.r,
                fill = rgb_hex(fill),
            )
        } else if let Some(stroke) = self.stroke {
            format!(
                r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{stroke}" stroke-width="2"/>"#,
                cx = self.cx,
                cy = self.cy,
                r = self.r,
                stroke = rgb_hex(stroke),
            )
        } else {
            String::new()
        }
    }
}

pub struct SvgDoc {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub font_family: String,
    pub font_size: f32,
    pub style_block: String,
    pub canvas_bg: [u8; 3],
    pub frame_bg_x: u32,
    pub frame_bg_y: u32,
    pub frame_bg_w: u32,
    pub frame_bg_h: u32,
    pub frame_bg_fill: [u8; 3],
    pub frame_clip_path: Option<(u32, u32, u32, u32, u32)>, // x, y, w, h, radius
    pub window_bar_circles: Vec<WindowBarCircle>,
    pub master_timer_dur: f32,
    pub bg_rects: Vec<BgRect>,
    pub text_elements: Vec<TextElement>,
    pub cursor_rects: Vec<CursorRect>,
}

fn is_static(start_ms: u32, end_ms: u32, total_ms: u32) -> bool {
    start_ms == 0 && end_ms >= total_ms
}

impl SvgDoc {
    pub fn to_svg(&self) -> String {
        // 1. Gather color classes
        let mut color_counts: HashMap<[u8; 3], usize> = HashMap::new();
        for bg in &self.bg_rects {
            *color_counts.entry(bg.fill).or_default() += 1;
        }
        for te in &self.text_elements {
            for tspan in &te.tspans {
                *color_counts.entry(tspan.fg).or_default() += 1;
            }
        }

        let mut color_classes: HashMap<[u8; 3], String> = HashMap::new();
        let mut class_id = 0;
        let mut sorted_colors: Vec<_> = color_counts.keys().cloned().collect();
        sorted_colors.sort();
        for color in sorted_colors {
            if color_counts[&color] >= 5 {
                color_classes.insert(color, format!("c{}", class_id));
                class_id += 1;
            }
        }

        // 2. Assemble style block
        let mut style = self.style_block.clone();
        if !color_classes.is_empty() {
            let mut color_css = String::new();
            color_css.push_str("<style>\n");
            let mut sorted_entries: Vec<_> = color_classes.iter().collect();
            sorted_entries
                .sort_by_key(|(_, class_name)| class_name[1..].parse::<usize>().unwrap_or(0));
            for (color, class_name) in sorted_entries {
                color_css.push_str(&format!(
                    ".{} {{ fill: {}; }}\n",
                    class_name,
                    rgb_hex(*color)
                ));
            }
            color_css.push_str("</style>\n");
            style.push_str(&color_css);
        }

        let mut s = String::with_capacity(128 * 1024);
        s.push_str(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="{font}" font-size="{fs}" xml:space="preserve">
{style}"#,
            w = self.canvas_w,
            h = self.canvas_h,
            font = escape_attr(&self.font_family),
            fs = self.font_size,
            style = style,
        ));

        // Canvas background
        s.push_str(&format!(
            r#"<rect width="{w}" height="{h}" fill="{bg}"/>
"#,
            w = self.canvas_w,
            h = self.canvas_h,
            bg = rgb_hex(self.canvas_bg),
        ));

        // Clip path
        if let Some((x, y, w, h, r)) = self.frame_clip_path {
            s.push_str(&format!(
                r#"<defs><clipPath id="frame-clip"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" ry="{r}"/></clipPath></defs>"#,
            ));
        }

        // Frame background
        let clip_attr = if self.frame_clip_path.is_some() {
            r#" clip-path="url(#frame-clip)""#
        } else {
            ""
        };
        s.push_str(&format!(
            r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{bg}"{clip}/>
"#,
            x = self.frame_bg_x,
            y = self.frame_bg_y,
            w = self.frame_bg_w,
            h = self.frame_bg_h,
            bg = rgb_hex(self.frame_bg_fill),
            clip = clip_attr,
        ));

        // Window bar circles
        for circle in &self.window_bar_circles {
            s.push_str(&circle.to_svg_string());
        }

        // Master timer
        s.push_str(&format!(
            r#"<rect width="0" height="0"><animate id="t" attributeName="x" from="0" to="0" dur="{dur}s" begin="0s;t.end"/></rect>
"#,
            dur = self.master_timer_dur
        ));

        // Background rects
        let total_ms = (self.master_timer_dur * 1000.0).round() as u32;
        for rect in &self.bg_rects {
            let fill_attr = if let Some(cls) = color_classes.get(&rect.fill) {
                format!(r#" class="{}""#, cls)
            } else {
                format!(r#" fill="{}""#, rgb_hex(rect.fill))
            };
            let rect_clip = if rect.clip_path.is_some() {
                clip_attr
            } else {
                ""
            };
            if is_static(rect.start_ms, rect.end_ms, total_ms) {
                s.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{w}" height="{h}"{fill}{clip}/>"#,
                    x = rect.x,
                    y = rect.y,
                    w = rect.w,
                    h = rect.h,
                    fill = fill_attr,
                    clip = rect_clip,
                ));
            } else {
                let begin_s = rect.start_ms as f32 / 1000.0;
                let end_s = rect.end_ms as f32 / 1000.0;
                s.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{w}" height="{h}"{fill}{clip} visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b:.2}s" end="t.begin+{e:.2}s"/></rect>"#,
                    x = rect.x,
                    y = rect.y,
                    w = rect.w,
                    h = rect.h,
                    fill = fill_attr,
                    clip = rect_clip,
                    b = begin_s,
                    e = end_s,
                ));
            }
        }

        // Text elements
        for te in &self.text_elements {
            s.push_str(&te.to_svg_string(&color_classes, total_ms));
        }

        // Cursor rects
        for cursor in &self.cursor_rects {
            if is_static(cursor.start_ms, cursor.end_ms, total_ms) {
                s.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{c}" fill-opacity="0.7"/>"#,
                    x = cursor.x,
                    y = cursor.y,
                    w = cursor.w,
                    h = cursor.h,
                    c = rgb_hex(cursor.fill),
                ));
            } else {
                let begin_s = cursor.start_ms as f32 / 1000.0;
                let end_s = cursor.end_ms as f32 / 1000.0;
                s.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{c}" fill-opacity="0.7" visibility="hidden"><set attributeName="visibility" to="visible" begin="t.begin+{b:.2}s" end="t.begin+{e:.2}s"/></rect>"#,
                    x = cursor.x,
                    y = cursor.y,
                    w = cursor.w,
                    h = cursor.h,
                    c = rgb_hex(cursor.fill),
                    b = begin_s,
                    e = end_s,
                ));
            }
        }

        s.push_str("\n</svg>\n");
        s
    }
}

pub fn optimize_tspans(elements: &mut [TextElement]) {
    for te in elements {
        if te.tspans.is_empty() {
            continue;
        }
        te.tspans
            .sort_by(|a, b| a.x_coords[0].partial_cmp(&b.x_coords[0]).unwrap());

        let mut merged: Vec<TSpan> = Vec::new();
        for tspan in std::mem::take(&mut te.tspans) {
            if let Some(last) = merged.last_mut() {
                if last.fg == tspan.fg
                    && last.bold == tspan.bold
                    && last.italic == tspan.italic
                    && last.underline == tspan.underline
                    && last.strikethrough == tspan.strikethrough
                    && last.is_box == tspan.is_box
                    && last.scale_y == tspan.scale_y
                    && last.cell_center_y_offset == tspan.cell_center_y_offset
                    && last.char_center_y_offset == tspan.char_center_y_offset
                    && last.letter_spacing == tspan.letter_spacing
                {
                    last.x_coords.extend(&tspan.x_coords);
                    last.text.push_str(&tspan.text);
                    continue;
                }
            }
            merged.push(tspan);
        }
        te.tspans = merged;
    }
}

pub fn group_text_elements_by_row_and_time(elements: &mut Vec<TextElement>) {
    if elements.is_empty() {
        return;
    }
    elements.sort_by(|a, b| {
        a.y.cmp(&b.y)
            .then(a.start_ms.cmp(&b.start_ms))
            .then(a.end_ms.cmp(&b.end_ms))
            .then_with(|| {
                let ax = a.tspans.first().map(|t| t.x_coords[0]).unwrap_or(0.0);
                let bx = b.tspans.first().map(|t| t.x_coords[0]).unwrap_or(0.0);
                ax.partial_cmp(&bx).unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut merged: Vec<TextElement> = Vec::new();
    for te in std::mem::take(elements) {
        if let Some(last) = merged.last_mut() {
            if last.y == te.y
                && last.start_ms == te.start_ms
                && last.end_ms == te.end_ms
            {
                last.tspans.extend(te.tspans);
                continue;
            }
        }
        merged.push(te);
    }
    *elements = merged;
}

pub fn optimize_bg_rects(bg_rects: &mut Vec<BgRect>) {
    if bg_rects.is_empty() {
        return;
    }
    bg_rects.sort_by(|a, b| {
        a.y.cmp(&b.y)
            .then(a.h.cmp(&b.h))
            .then(a.fill.cmp(&b.fill))
            .then(a.start_ms.cmp(&b.start_ms))
            .then(a.end_ms.cmp(&b.end_ms))
            .then(a.clip_path.cmp(&b.clip_path))
            .then(a.x.cmp(&b.x))
    });

    let mut merged: Vec<BgRect> = Vec::new();
    for rect in std::mem::take(bg_rects) {
        if let Some(last) = merged.last_mut() {
            if last.y == rect.y
                && last.h == rect.h
                && last.fill == rect.fill
                && last.start_ms == rect.start_ms
                && last.end_ms == rect.end_ms
                && last.clip_path == rect.clip_path
                && last.x + last.w == rect.x
            {
                last.w += rect.w;
                continue;
            }
        }
        merged.push(rect);
    }
    *bg_rects = merged;
}

pub fn optimize_rows(elements: &mut Vec<TextElement>) {
    elements.sort_by_key(|e| e.start_ms);

    let mut i = 0;
    while i < elements.len() {
        let mut merged = false;
        for j in (i + 1)..elements.len() {
            if elements[i].content_equals(&elements[j]) {
                if elements[i].end_ms == elements[j].start_ms {
                    let el2_y = elements[j].y;
                    let el2_start = elements[j].start_ms;
                    let el2_end = elements[j].end_ms;

                    let el1 = &mut elements[i];
                    el1.end_ms = el2_end;
                    if let Some(ref mut anim) = el1.y_animation {
                        anim.segments.push((el2_y, el2_start));
                        anim.dur_ms = el2_end - anim.begin_ms;
                    } else {
                        el1.y_animation = Some(YAnimation {
                            begin_ms: el1.start_ms,
                            segments: vec![(el1.y, el1.start_ms), (el2_y, el2_start)],
                            dur_ms: el2_end - el1.start_ms,
                        });
                    }
                    elements.remove(j);
                    merged = true;
                    break;
                }
            }
        }
        if !merged {
            i += 1;
        }
    }
}

fn render_from_frames(
    frames: &[RawFrame],
    cfg: ViewportConfig,
    opts: &SvgOptions,
) -> Result<String> {
    let canvas_w = cfg.canvas_w;
    let canvas_h = cfg.canvas_h;

    if frames.is_empty() {
        return Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">
</svg>
"#,
            w = canvas_w,
            h = canvas_h,
        ));
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
            if *old_visual != new_visual
                || (old_visual.bg == old_default_bg
                    && new_visual.bg == frame.default_bg
                    && old_default_bg != frame.default_bg)
            {
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
    let mut cur_cursor: Option<(u16, u16, u32, [u8; 3])> = None;

    for frame in frames.iter() {
        let cc = frame.cursor_color.unwrap_or(frame.default_fg);
        match (cur_cursor, frame.cursor) {
            (None, Some((cx, cy))) => {
                cur_cursor = Some((cx, cy, frame.t_ms, cc));
            }
            (Some((ocx, ocy, start, color)), Some((cx, cy))) => {
                if ocx != cx || ocy != cy || color != cc {
                    cursor_spans.push(CursorSpan {
                        col: ocx,
                        row: ocy,
                        start_ms: start,
                        end_ms: frame.t_ms,
                        color,
                    });
                    cur_cursor = Some((cx, cy, frame.t_ms, cc));
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
    let mut font_family = opts.font_family.clone();
    if let Some(ref path) = opts.font_path {
        if let Ok(loaded) = load_font_family(Some(path)) {
            let primary = &loaded.font_set.fonts[loaded.font_set.regular[0]];
            font_family = format!("'{}', {}", primary.family_name, font_family);
        }
    }

    let cell_w = cfg.cell_width_px.max(1);
    let cell_h = cfg.cell_height_px.max(1);

    assert!(
        cfg.char_height_px > 0 && cfg.font_size_px > 0.0,
        "font metrics are always required"
    );
    let scale = opts.font_size / cfg.font_size_px;
    let letter_spacing_svg = cfg.letter_spacing * scale;
    let offset_x_svg = (letter_spacing_svg / 2.0).floor();

    let baseline = {
        let char_h_svg = cfg.char_height_px as f32 * scale;
        let ascent_svg = cfg.ascent_px as f32 * scale;
        let extra = (cell_h as f32 - char_h_svg).max(0.0);
        (ascent_svg + extra / 2.0).round() as u32
    };

    // Construct SVG Doc structures
    let mut bg_rects = Vec::new();
    for span in &cell_spans {
        if span.start_ms == span.end_ms {
            continue;
        }
        if span.visual.bg == span.default_bg {
            continue;
        }
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h;
        bg_rects.push(BgRect {
            x,
            y,
            w: cell_w,
            h: cell_h,
            fill: span.visual.bg,
            start_ms: span.start_ms,
            end_ms: span.end_ms,
            clip_path: if cfg.frame_style.border_radius_px > 0 {
                Some("url(#frame-clip)".to_string())
            } else {
                None
            },
        });
    }

    let mut text_elements = Vec::new();
    for span in &cell_spans {
        if span.start_ms == span.end_ms {
            continue;
        }
        if span.visual.text.is_empty() {
            continue;
        }
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h + baseline;

        let is_box = span.visual.text.chars().any(is_box_drawing);
        let mut scale_y = 1.0;
        let mut cell_center_y_offset = 0.0;
        let mut char_center_y_offset = 0.0;

        if is_box {
            let scale_font = opts.font_size / cfg.font_size_px;
            let char_h_svg = cfg.char_height_px as f32 * scale_font;
            let ascent_svg = cfg.ascent_px as f32 * scale_font;
            scale_y = (cell_h as f32 / char_h_svg).max(1.0);

            cell_center_y_offset = -(baseline as f32) + (cell_h as f32 / 2.0);
            char_center_y_offset = -ascent_svg + (char_h_svg / 2.0);
        }

        let draw_x = if is_box {
            x as f32
        } else {
            x as f32 + offset_x_svg
        };

        let char_count = span.visual.text.chars().count();
        let mut x_coords = Vec::with_capacity(char_count);
        x_coords.push(draw_x);
        for i in 1..char_count {
            x_coords.push(draw_x + (i as f32 * cell_w as f32 / char_count as f32));
        }

        let tspan = TSpan {
            x_coords,
            text: span.visual.text.clone(),
            fg: span.visual.fg,
            bold: span.visual.flags & style_flags::BOLD != 0,
            italic: span.visual.flags & style_flags::ITALIC != 0,
            underline: span.visual.flags & style_flags::UNDERLINE != 0,
            strikethrough: span.visual.flags & style_flags::STRIKETHROUGH != 0,
            is_box,
            scale_y,
            cell_center_y_offset,
            char_center_y_offset,
            cell_w,
            cell_h,
            baseline,
            letter_spacing: if is_box { 0.0 } else { letter_spacing_svg },
        };

        text_elements.push(TextElement {
            y,
            y_animation: None,
            start_ms: span.start_ms,
            end_ms: span.end_ms,
            tspans: vec![tspan],
        });
    }

    let mut cursor_rects = Vec::new();
    for span in &cursor_spans {
        if span.start_ms == span.end_ms {
            continue;
        }
        let x = cfg.content_x + span.col as u32 * cell_w;
        let y = cfg.content_y + span.row as u32 * cell_h;
        cursor_rects.push(CursorRect {
            x,
            y,
            w: cell_w,
            h: cell_h,
            fill: span.color,
            start_ms: span.start_ms,
            end_ms: span.end_ms,
        });
    }

    // Run optimizations
    group_text_elements_by_row_and_time(&mut text_elements);
    optimize_tspans(&mut text_elements);
    optimize_bg_rects(&mut bg_rects);
    optimize_rows(&mut text_elements);

    let mut window_bar_circles = Vec::new();
    if cfg.frame_style.window_bar.enabled() {
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
                window_bar_circles.push(WindowBarCircle {
                    cx,
                    cy,
                    r: radius,
                    fill: None,
                    stroke: Some(*color),
                });
            } else {
                window_bar_circles.push(WindowBarCircle {
                    cx,
                    cy,
                    r: radius,
                    fill: Some(*color),
                    stroke: None,
                });
            }
        }
    }

    let doc = SvgDoc {
        canvas_w,
        canvas_h,
        font_family,
        font_size: opts.font_size,
        style_block: generate_style_block(frames, opts)?,
        canvas_bg: cfg.frame_style.margin_fill,
        frame_bg_x: cfg.frame_x,
        frame_bg_y: cfg.frame_y,
        frame_bg_w: cfg.frame_w,
        frame_bg_h: cfg.frame_h,
        frame_bg_fill: frames[0].default_bg,
        frame_clip_path: if cfg.frame_style.border_radius_px > 0 {
            Some((
                cfg.frame_x,
                cfg.frame_y,
                cfg.frame_w,
                cfg.frame_h,
                cfg.frame_style
                    .border_radius_px
                    .min(cfg.frame_w / 2)
                    .min(cfg.frame_h / 2),
            ))
        } else {
            None
        },
        window_bar_circles,
        master_timer_dur: total_s,
        bg_rects,
        text_elements,
        cursor_rects,
    };

    Ok(doc.to_svg())
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

    let s = render_from_frames(&frames, cfg, &opts)?;

    let is_svgz = out
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svgz"));

    let mut file = File::create(&out).with_context(|| format!("create {}", out.display()))?;
    if is_svgz {
        let mut encoder = GzEncoder::new(file, Compression::best());
        encoder
            .write_all(s.as_bytes())
            .with_context(|| format!("writing gzipped {}", out.display()))?;
        encoder.finish().context("finalising gzip compression")?;
    } else {
        file.write_all(s.as_bytes())
            .with_context(|| format!("writing {}", out.display()))?;
    }
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
            // Plausible metrics for JetBrains Mono at the SVG default font_size
            // of 16px: bbox_h ≈ 19px, ascent ≈ 15px.
            font_size_px: 16.0,
            char_height_px: 19,
            ascent_px: 15,
            letter_spacing: 1.0,
            frame_style: FrameStyle {
                padding_px: 4,
                ..FrameStyle::default()
            },
            frames: vec![Frame::Key {
                t_ms: 0,
                cursor: Some((2, 0)),
                default_fg: [255, 255, 255],
                default_bg: [0, 0, 0],
                cursor_color: None,
                cursor_accent: None,
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

    #[test]
    fn test_render_svg_and_svgz() {
        let rec = synth_recording();
        let temp_dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let svg_path = temp_dir.join(format!("evp_test_{}.svg", stamp));
        let svgz_path = temp_dir.join(format!("evp_test_{}.svgz", stamp));

        render_svg(&rec, &SvgOptions::default(), &svg_path).unwrap();
        render_svg(&rec, &SvgOptions::default(), &svgz_path).unwrap();

        assert!(svg_path.exists());
        assert!(svgz_path.exists());

        let svg_bytes = std::fs::read(&svg_path).unwrap();
        let svgz_bytes = std::fs::read(&svgz_path).unwrap();

        // Gzipped data has 0x1f 0x8b magic number at start.
        assert!(svgz_bytes.len() < svg_bytes.len());
        assert_eq!(svgz_bytes[0], 0x1f);
        assert_eq!(svgz_bytes[1], 0x8b);

        std::fs::remove_file(svg_path).ok();
        std::fs::remove_file(svgz_path).ok();
    }

    #[test]
    fn test_font_subset_fallback_on_cff_cjk() {
        let mut rec = synth_recording();
        // Insert a CJK character to trigger the Noto Sans Mono CJK JP fallback font
        if let Frame::Key { cells, .. } = &mut rec.frames[0] {
            cells[2] = CellSnap {
                text: "あ".into(),
                fg: [255, 255, 255],
                bg: [0, 0, 0],
                flags: 0,
            };
        }

        let result = render_svg_to_string(&rec, &SvgOptions::default());
        assert!(
            result.is_ok(),
            "Rendering SVG with CJK characters should succeed via full font fallback"
        );
        let svg = result.unwrap();
        assert!(svg.contains("font-family: 'Noto Sans Mono CJK JP'"));
        assert!(svg.contains("url(data:font/woff2;base64,"));
    }

    #[test]
    fn test_optimize_tspans() {
        let mut te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 0,
            end_ms: 1000,
            tspans: vec![
                TSpan {
                    x_coords: vec![10.0],
                    text: "a".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                },
                TSpan {
                    x_coords: vec![20.0],
                    text: "b".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                },
            ],
        };
        optimize_tspans(std::slice::from_mut(&mut te));
        assert_eq!(te.tspans.len(), 1);
        assert_eq!(te.tspans[0].text, "ab");
        assert_eq!(te.tspans[0].x_coords, vec![10.0, 20.0]);
    }

    #[test]
    fn test_optimize_bg_rects() {
        let mut rects = vec![
            BgRect {
                x: 10,
                y: 20,
                w: 10,
                h: 20,
                fill: [255, 0, 0],
                start_ms: 0,
                end_ms: 1000,
                clip_path: None,
            },
            BgRect {
                x: 20,
                y: 20,
                w: 10,
                h: 20,
                fill: [255, 0, 0],
                start_ms: 0,
                end_ms: 1000,
                clip_path: None,
            },
        ];
        optimize_bg_rects(&mut rects);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 10);
        assert_eq!(rects[0].w, 20);
    }

    #[test]
    fn test_optimize_rows() {
        let mut te_list = vec![
            TextElement {
                y: 20,
                y_animation: None,
                start_ms: 0,
                end_ms: 500,
                tspans: vec![TSpan {
                    x_coords: vec![10.0],
                    text: "hello".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                }],
            },
            TextElement {
                y: 40,
                y_animation: None,
                start_ms: 500,
                end_ms: 1000,
                tspans: vec![TSpan {
                    x_coords: vec![10.0],
                    text: "hello".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                }],
            },
        ];
        optimize_rows(&mut te_list);
        assert_eq!(te_list.len(), 1);
        assert_eq!(te_list[0].start_ms, 0);
        assert_eq!(te_list[0].end_ms, 1000);
        let anim = te_list[0].y_animation.as_ref().unwrap();
        assert_eq!(anim.begin_ms, 0);
        assert_eq!(anim.dur_ms, 1000);
        assert_eq!(anim.segments, vec![(20, 0), (40, 500)]);
    }

    #[test]
    fn test_group_text_elements_by_row_and_time() {
        let mut elements = vec![
            TextElement {
                y: 20,
                y_animation: None,
                start_ms: 100,
                end_ms: 200,
                tspans: vec![TSpan {
                    x_coords: vec![10.0],
                    text: "a".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                }],
            },
            TextElement {
                y: 20,
                y_animation: None,
                start_ms: 100,
                end_ms: 200,
                tspans: vec![TSpan {
                    x_coords: vec![20.0],
                    text: "b".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                }],
            },
            TextElement {
                y: 40,
                y_animation: None,
                start_ms: 100,
                end_ms: 200,
                tspans: vec![TSpan {
                    x_coords: vec![10.0],
                    text: "c".to_string(),
                    fg: [255, 0, 0],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: false,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                }],
            },
        ];

        group_text_elements_by_row_and_time(&mut elements);
        assert_eq!(elements.len(), 2);
        
        // Element at y=20 (a and b should be merged)
        assert_eq!(elements[0].y, 20);
        assert_eq!(elements[0].start_ms, 100);
        assert_eq!(elements[0].end_ms, 200);
        assert_eq!(elements[0].tspans.len(), 2);
        assert_eq!(elements[0].tspans[0].text, "a");
        assert_eq!(elements[0].tspans[1].text, "b");

        // Element at y=40 (c should remain separate)
        assert_eq!(elements[1].y, 40);
        assert_eq!(elements[1].tspans.len(), 1);
        assert_eq!(elements[1].tspans[0].text, "c");
    }
}
