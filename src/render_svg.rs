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

use crate::font::SvgFontEmbeddingPolicy;
use crate::font::load_font_family;
use ab_glyph::Font;
use base64::prelude::*;
use std::collections::{BTreeSet, HashMap};
use unicode_width::UnicodeWidthChar;

use crate::{
    recording::{RawFrame, Recording, style_flags},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, ViewportConfig},
    style::{rgb_hex, window_bar_dot_metrics},
};

fn generate_style_block(frames: &[RawFrame], opts: &SvgOptions) -> Result<String> {
    let loaded = load_font_family(opts.font_path.as_deref())?;

    if opts.no_system_fonts {
        for frame in frames {
            for cell in &frame.cells {
                for c in cell.text.chars() {
                    let (_, font) = loaded.font_set.select_for_char(cell.flags, c);
                    if font.glyph_id(c).0 == 0 {
                        return Err(anyhow::anyhow!(
                            "Glyph not found in embedded fonts for character '{}' (U+{:04X})",
                            c,
                            c as u32
                        ));
                    }
                }
            }
        }
    }

    if !opts.embed_fonts {
        return Ok(String::new());
    }

    let mut style = String::new();
    style.push_str("<style>\n");

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

        let mut embed_comment = None;
        let (font_bytes, format_str, mime_type) = match info.svg_embedding_policy() {
            Ok(SvgFontEmbeddingPolicy::AllowSubsetting) => match info.subset(chars) {
                Ok(subset) => (subset, "woff2", "font/woff2"),
                Err(err) => {
                    tracing::warn!(
                        "failed to subset font '{}' ({} chars), embedding the entire font: {:?}",
                        info.family_name,
                        chars.len(),
                        err
                    );
                    embed_comment = Some(format!(
                        "/* Font subsetting failed for '{}'; embedding the full font data in this SVG. */\n",
                        info.family_name
                    ));
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
            },
            Ok(SvgFontEmbeddingPolicy::EmbedFullFont { reason }) => {
                embed_comment = Some(format!("/* {} */\n", reason));
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
            Ok(SvgFontEmbeddingPolicy::OmitFont { reason }) => {
                style.push_str(&format!("/* {} */\n", reason));
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    "failed to inspect font permissions for '{}' ({} chars), embedding the entire font: {:?}",
                    info.family_name,
                    chars.len(),
                    err
                );
                embed_comment = Some(format!(
                    "/* Font permission check failed for '{}'; embedding the full font data in this SVG. */\n",
                    info.family_name
                ));
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

        if let Some(comment) = embed_comment {
            style.push_str(&comment);
        }

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
    /// Whether to exclude system fonts from the font-family stack.
    pub no_system_fonts: bool,
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
            font_family: "'JetBrainsMono Nerd Font Mono', 'Noto Sans Mono', 'Noto Emoji', 'Noto Sans Symbols 2', 'Noto Sans Mono CJK JP', 'unifont_upper', 'unifont_csur', ui-monospace, Menlo, Consolas, 'DejaVu Sans Mono', monospace".to_string(),
            font_size: 16.0,
            embed_fonts: true,
            no_system_fonts: false,
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

fn is_wide_char(ch: char) -> bool {
    ch.width() == Some(2)
}

fn get_frame_visuals(frame: &RawFrame) -> Vec<CellVisual> {
    let cols = frame.cols as usize;
    let rows = frame.rows as usize;
    let default_bg = frame.default_bg;
    let mut visuals = Vec::with_capacity(frame.cells.len());

    for r in 0..rows {
        let mut last_active = None;
        for c in (0..cols).rev() {
            let idx = r * cols + c;
            let cell = &frame.cells[idx];
            if !cell.text.is_empty() || cell.bg != default_bg {
                last_active = Some(c);
                break;
            }
        }

        for c in 0..cols {
            let idx = r * cols + c;
            let cell = &frame.cells[idx];
            let mut visual = CellVisual::from_snap(cell);

            let prev_is_wide = if c > 0 {
                let prev_idx = r * cols + (c - 1);
                let prev_cell = &frame.cells[prev_idx];
                prev_cell
                    .text
                    .chars()
                    .next()
                    .map(is_wide_char)
                    .unwrap_or(false)
            } else {
                false
            };

            if visual.text.is_empty()
                && last_active.is_some()
                && c <= last_active.unwrap()
                && !prev_is_wide
            {
                visual.text = " ".to_string();
            }
            visuals.push(visual);
        }
    }
    visuals
}

/// A time span during which a cell has a particular visual state.
#[derive(Clone)]
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
pub struct StyleKeyframe {
    pub start_ms: u32,
    pub fg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnimatedProperty {
    Fg(Vec<(u32, [u8; 3])>),
    FontWeight(Vec<(u32, bool)>),
    FontStyle(Vec<(u32, bool)>),
    TextDecoration(Vec<(u32, (bool, bool))>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StyleAnimation {
    pub begin_ms: u32,
    pub dur_ms: u32,
    pub property: AnimatedProperty,
}

impl StyleAnimation {
    fn to_svg_string(&self, color_classes: &HashMap<[u8; 3], String>, is_hidden: bool) -> String {
        let (attr_name, values): (&str, Vec<String>) = match &self.property {
            AnimatedProperty::Fg(keyframes) => {
                let use_class = keyframes
                    .iter()
                    .all(|(_, color)| color_classes.contains_key(color));
                if use_class {
                    let vals = keyframes
                        .iter()
                        .map(|(_, color)| color_classes.get(color).cloned().unwrap_or_default())
                        .collect();
                    ("class", vals)
                } else {
                    let vals = keyframes.iter().map(|(_, color)| rgb_hex(*color)).collect();
                    ("fill", vals)
                }
            }
            AnimatedProperty::FontWeight(keyframes) => {
                let vals = keyframes
                    .iter()
                    .map(|(_, bold)| {
                        if *bold {
                            "bold".to_string()
                        } else {
                            "normal".to_string()
                        }
                    })
                    .collect();
                ("font-weight", vals)
            }
            AnimatedProperty::FontStyle(keyframes) => {
                let vals = keyframes
                    .iter()
                    .map(|(_, italic)| {
                        if *italic {
                            "italic".to_string()
                        } else {
                            "normal".to_string()
                        }
                    })
                    .collect();
                ("font-style", vals)
            }
            AnimatedProperty::TextDecoration(keyframes) => {
                let vals = keyframes
                    .iter()
                    .map(|(_, (u, s))| {
                        if *u && *s {
                            "underline line-through".to_string()
                        } else if *u {
                            "underline".to_string()
                        } else if *s {
                            "line-through".to_string()
                        } else {
                            "none".to_string()
                        }
                    })
                    .collect();
                ("text-decoration", vals)
            }
        };

        let key_times_str = match &self.property {
            AnimatedProperty::Fg(kf) => kf
                .iter()
                .map(|(t, _)| format_key_time((*t - self.begin_ms) as f32 / self.dur_ms as f32))
                .collect::<Vec<_>>()
                .join(";"),
            AnimatedProperty::FontWeight(kf) => kf
                .iter()
                .map(|(t, _)| format_key_time((*t - self.begin_ms) as f32 / self.dur_ms as f32))
                .collect::<Vec<_>>()
                .join(";"),
            AnimatedProperty::FontStyle(kf) => kf
                .iter()
                .map(|(t, _)| format_key_time((*t - self.begin_ms) as f32 / self.dur_ms as f32))
                .collect::<Vec<_>>()
                .join(";"),
            AnimatedProperty::TextDecoration(kf) => kf
                .iter()
                .map(|(t, _)| format_key_time((*t - self.begin_ms) as f32 / self.dur_ms as f32))
                .collect::<Vec<_>>()
                .join(";"),
        };

        let values_str = values.join(";");
        let fill_freeze = if is_hidden { "" } else { r#" fill="freeze""# };

        format!(
            r#"<animate attributeName="{attr}" calcMode="discrete" values="{values}" keyTimes="{key_times}" dur="{dur}" begin="{begin}"{freeze} />"#,
            attr = attr_name,
            values = values_str,
            key_times = key_times_str,
            dur = format_time(self.dur_ms),
            begin = format_begin(self.begin_ms),
            freeze = fill_freeze,
        )
    }
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
    pub start_ms: u32,
    pub end_ms: u32,
    pub style_animations: Vec<StyleAnimation>,
    pub style_history: Vec<StyleKeyframe>,
}

impl TSpan {
    fn to_svg_string(
        &self,
        color_classes: &HashMap<[u8; 3], String>,
        parent_start_ms: u32,
        parent_end_ms: u32,
        total_ms: u32,
        default_letter_spacing: f32,
        default_fg: Option<[u8; 3]>,
        make_transparent: bool,
    ) -> String {
        let x_str = self
            .x_coords
            .iter()
            .map(|x| {
                let formatted = format!("{:.2}", x);
                let mut trimmed = formatted.trim_end_matches('0');
                if trimmed.ends_with('.') {
                    trimmed = &trimmed[..trimmed.len() - 1];
                }
                trimmed.to_string()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut styles = Vec::new();
        if (self.letter_spacing - default_letter_spacing).abs() >= 1e-5 {
            styles.push(format!(
                "letter-spacing: {}px",
                format_seconds(self.letter_spacing)
            ));
        }
        if make_transparent {
            styles.push("fill-opacity: 0".to_string());
        }
        let style = if styles.is_empty() {
            String::new()
        } else {
            format!(r#" style="{}""#, styles.join("; "))
        };

        let is_tspan_static = is_static(self.start_ms, self.end_ms, total_ms);
        let matches_parent = self.start_ms <= parent_start_ms && self.end_ms >= parent_end_ms;
        let is_hidden = !is_tspan_static && !matches_parent;

        let mut style_classes = Vec::new();
        if self.bold {
            style_classes.push("b");
        }
        if self.italic {
            style_classes.push("i");
        }
        if self.underline && self.strikethrough {
            style_classes.push("us");
        } else if self.underline {
            style_classes.push("u");
        } else if self.strikethrough {
            style_classes.push("s");
        }
        if is_hidden {
            style_classes.push("h");
        }

        let mut fill_attr = String::new();
        if let Some(cls) = color_classes.get(&self.fg) {
            style_classes.push(cls);
        } else if Some(self.fg) != default_fg {
            fill_attr = format!(r#" fill="{}""#, rgb_hex(self.fg));
        }

        let class_attr = if !style_classes.is_empty() {
            format!(r#" class="{}""#, style_classes.join(" "))
        } else {
            String::new()
        };

        let set_str = if is_hidden {
            visibility_set(self.start_ms, self.end_ms, total_ms)
        } else {
            String::new()
        };

        let mut anim_str = String::new();
        for anim in &self.style_animations {
            anim_str.push_str(&anim.to_svg_string(color_classes, is_hidden));
        }

        format!(
            r#"<tspan x="{x}"{fill}{cls}{s}>{set}{anim}{txt}</tspan>"#,
            x = x_str,
            fill = fill_attr,
            cls = class_attr,
            s = style,
            set = set_str,
            anim = anim_str,
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
            .map(|(_, start)| format_key_time((start - self.begin_ms) as f32 / self.dur_ms as f32))
            .collect::<Vec<_>>()
            .join(";");
        format!(
            r#"<animate attributeName="y" calcMode="discrete" values="{values}" keyTimes="{key_times}" dur="{dur}" begin="{begin}" fill="freeze"/>"#,
            values = values_str,
            key_times = key_times_str,
            dur = format_time(self.dur_ms),
            begin = format_begin(self.begin_ms),
        )
    }

    fn to_svg_transform_animation(&self, cell_x: f32, baseline: u32) -> String {
        let values_str = self
            .segments
            .iter()
            .map(|(y, _)| format!("{lx},{ly}", lx = cell_x, ly = *y as f32 - baseline as f32))
            .collect::<Vec<_>>()
            .join(";");
        let key_times_str = self
            .segments
            .iter()
            .map(|(_, start)| format_key_time((start - self.begin_ms) as f32 / self.dur_ms as f32))
            .collect::<Vec<_>>()
            .join(";");
        format!(
            r#"<animateTransform attributeName="transform" type="translate" calcMode="discrete" values="{values}" keyTimes="{key_times}" dur="{dur}" begin="{begin}" fill="freeze"/>"#,
            values = values_str,
            key_times = key_times_str,
            dur = format_time(self.dur_ms),
            begin = format_begin(self.begin_ms),
        )
    }
}

fn get_box_drawing_path(
    ch: char,
    cell_w: f32,
    cell_h: f32,
    lw: f32,
    hw: f32,
    d_off: f32,
    r: f32,
) -> Option<String> {
    let cx = cell_w / 2.0;
    let cy = cell_h / 2.0;
    let rx = cell_w;
    let by = cell_h;

    match ch {
        // --- Single lines ---
        '─' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{lw}" stroke-linecap="butt"/>"#
        )),
        '━' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{hw}" stroke-linecap="butt"/>"#
        )),
        '│' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{lw}" stroke-linecap="butt"/>"#
        )),
        '┃' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{hw}" stroke-linecap="butt"/>"#
        )),

        // --- Dashed lines ---
        '┄' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{lw}" stroke-dasharray="3,3" stroke-linecap="butt"/>"#
        )),
        '┅' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{hw}" stroke-dasharray="3,3" stroke-linecap="butt"/>"#
        )),
        '┆' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{lw}" stroke-dasharray="3,3" stroke-linecap="butt"/>"#
        )),
        '┇' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{hw}" stroke-dasharray="3,3" stroke-linecap="butt"/>"#
        )),
        '┈' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{lw}" stroke-dasharray="2,2" stroke-linecap="butt"/>"#
        )),
        '┉' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{hw}" stroke-dasharray="2,2" stroke-linecap="butt"/>"#
        )),
        '┊' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{lw}" stroke-dasharray="2,2" stroke-linecap="butt"/>"#
        )),
        '┋' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{hw}" stroke-dasharray="2,2" stroke-linecap="butt"/>"#
        )),

        // --- Corners (Light) ---
        '┌' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx} V {by}" fill="none" stroke-width="{lw}" stroke-linejoin="miter"/>"#
        )),
        '┐' => Some(format!(
            r#"<path d="M 0 {cy} H {cx} V {by}" fill="none" stroke-width="{lw}" stroke-linejoin="miter"/>"#
        )),
        '└' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx} V 0" fill="none" stroke-width="{lw}" stroke-linejoin="miter"/>"#
        )),
        '┘' => Some(format!(
            r#"<path d="M 0 {cy} H {cx} V 0" fill="none" stroke-width="{lw}" stroke-linejoin="miter"/>"#
        )),

        // --- Corners (Heavy) ---
        '┏' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx} V {by}" fill="none" stroke-width="{hw}" stroke-linejoin="miter"/>"#
        )),
        '┓' => Some(format!(
            r#"<path d="M 0 {cy} H {cx} V {by}" fill="none" stroke-width="{hw}" stroke-linejoin="miter"/>"#
        )),
        '┗' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx} V 0" fill="none" stroke-width="{hw}" stroke-linejoin="miter"/>"#
        )),
        '┛' => Some(format!(
            r#"<path d="M 0 {cy} H {cx} V 0" fill="none" stroke-width="{hw}" stroke-linejoin="miter"/>"#
        )),

        // --- Corners (Mixed Down/Right) ---
        '┍' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┎' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{hw}"/>"#
        )),
        // --- Corners (Mixed Down/Left) ---
        '┑' => Some(format!(
            r#"<path d="M 0 {cy} H {cx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┒' => Some(format!(
            r#"<path d="M 0 {cy} H {cx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{hw}"/>"#
        )),
        // --- Corners (Mixed Up/Right) ---
        '┕' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{lw}"/>"#
        )),
        '┖' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{hw}"/>"#
        )),
        // --- Corners (Mixed Up/Left) ---
        '┙' => Some(format!(
            r#"<path d="M 0 {cy} H {cx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{lw}"/>"#
        )),
        '┚' => Some(format!(
            r#"<path d="M 0 {cy} H {cx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{hw}"/>"#
        )),

        // --- Rounded Corners ---
        '╭' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx_r} Q {cx} {cy} {cx} {cy_r} V {by}" fill="none" stroke-width="{lw}" stroke-linejoin="round"/>"#,
            cx_r = cx + r,
            cy_r = cy + r
        )),
        '╮' => Some(format!(
            r#"<path d="M 0 {cy} H {cx_r} Q {cx} {cy} {cx} {cy_r} V {by}" fill="none" stroke-width="{lw}" stroke-linejoin="round"/>"#,
            cx_r = cx - r,
            cy_r = cy + r
        )),
        '╯' => Some(format!(
            r#"<path d="M 0 {cy} H {cx_r} Q {cx} {cy} {cx} {cy_r} V 0" fill="none" stroke-width="{lw}" stroke-linejoin="round"/>"#,
            cx_r = cx - r,
            cy_r = cy - r
        )),
        '╰' => Some(format!(
            r#"<path d="M {rx} {cy} H {cx_r} Q {cx} {cy} {cx} {cy_r} V 0" fill="none" stroke-width="{lw}" stroke-linejoin="round"/>"#,
            cx_r = cx + r,
            cy_r = cy - r
        )),

        // --- Tees (Light) ---
        '├' => Some(format!(
            r#"<path d="M {cx} 0 V {by} M {cx} {cy} H {rx}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┤' => Some(format!(
            r#"<path d="M {cx} 0 V {by} M {cx} {cy} H 0" fill="none" stroke-width="{lw}"/>"#
        )),
        '┬' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} {cy} V {by}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┴' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} {cy} V 0" fill="none" stroke-width="{lw}"/>"#
        )),
        '┼' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} 0 V {by}" fill="none" stroke-width="{lw}"/>"#
        )),

        // --- Tees (Heavy) ---
        '┣' => Some(format!(
            r#"<path d="M {cx} 0 V {by} M {cx} {cy} H {rx}" fill="none" stroke-width="{hw}"/>"#
        )),
        '┫' => Some(format!(
            r#"<path d="M {cx} 0 V {by} M {cx} {cy} H 0" fill="none" stroke-width="{hw}"/>"#
        )),
        '┳' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} {cy} V {by}" fill="none" stroke-width="{hw}"/>"#
        )),
        '┻' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} {cy} V 0" fill="none" stroke-width="{hw}"/>"#
        )),
        '╋' => Some(format!(
            r#"<path d="M 0 {cy} H {rx} M {cx} 0 V {by}" fill="none" stroke-width="{hw}"/>"#
        )),

        // --- Mixed Tees (Light/Heavy/Vertical/Horizontal) ---
        '┠' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} H {rx}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┨' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} H 0" fill="none" stroke-width="{lw}"/>"#
        )),
        '┰' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{lw}"/>"#
        )),
        '┸' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{hw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{lw}"/>"#
        )),

        '┝' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} H {rx}" fill="none" stroke-width="{hw}"/>"#
        )),
        '┥' => Some(format!(
            r#"<path d="M {cx} 0 V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} H 0" fill="none" stroke-width="{hw}"/>"#
        )),
        '┯' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V {by}" fill="none" stroke-width="{hw}"/>"#
        )),
        '┷' => Some(format!(
            r#"<path d="M 0 {cy} H {rx}" fill="none" stroke-width="{lw}"/>
               <path d="M {cx} {cy} V 0" fill="none" stroke-width="{hw}"/>"#
        )),

        // --- Double lines ---
        '═' => Some(format!(
            r#"<path d="M 0 {y1} H {rx} M 0 {y2} H {rx}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
        )),
        '║' => Some(format!(
            r#"<path d="M {x1} 0 V {by} M {x2} 0 V {by}" fill="none" stroke-width="{lw}"/>"#,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╔' => Some(format!(
            r#"<path d="M {rx} {y1} H {x1} V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y2} H {x2} V {by}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╗' => Some(format!(
            r#"<path d="M 0 {y1} H {x2} V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M 0 {y2} H {x1} V {by}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╚' => Some(format!(
            r#"<path d="M {rx} {y2} H {x1} V 0" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y1} H {x2} V 0" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╝' => Some(format!(
            r#"<path d="M 0 {y2} H {x2} V 0" fill="none" stroke-width="{lw}"/>
               <path d="M 0 {y1} H {x1} V 0" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╠' => Some(format!(
            r#"<path d="M {x1} 0 V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {x2} 0 V {y1} H {rx}" fill="none" stroke-width="{lw}"/>
               <path d="M {x2} {by} V {y2} H {rx}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╣' => Some(format!(
            r#"<path d="M {x2} 0 V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {x1} 0 V {y1} H 0" fill="none" stroke-width="{lw}"/>
               <path d="M {x1} {by} V {y2} H 0" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╦' => Some(format!(
            r#"<path d="M 0 {y1} H {rx}" fill="none" stroke-width="{lw}"/>
               <path d="M 0 {y2} H {x1} V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y2} H {x2} V {by}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╩' => Some(format!(
            r#"<path d="M 0 {y2} H {rx}" fill="none" stroke-width="{lw}"/>
               <path d="M 0 {y1} H {x1} V 0" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y1} H {x2} V 0" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),
        '╬' => Some(format!(
            r#"<path d="M 0 {y1} H {x1} V 0" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y1} H {x2} V 0" fill="none" stroke-width="{lw}"/>
               <path d="M 0 {y2} H {x1} V {by}" fill="none" stroke-width="{lw}"/>
               <path d="M {rx} {y2} H {x2} V {by}" fill="none" stroke-width="{lw}"/>"#,
            y1 = cy - d_off,
            y2 = cy + d_off,
            x1 = cx - d_off,
            x2 = cx + d_off,
        )),

        // --- Block Elements ---
        '█' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cell_h}" stroke="none"/>"#
        )),
        '▀' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{y_half}" stroke="none"/>"#,
            y_half = cell_h / 2.0
        )),
        '▄' => Some(format!(
            r#"<rect x="0" y="{y_half}" width="{cell_w}" height="{y_half}" stroke="none"/>"#,
            y_half = cell_h / 2.0
        )),
        '▌' => Some(format!(
            r#"<rect x="0" y="0" width="{x_half}" height="{cell_h}" stroke="none"/>"#,
            x_half = cell_w / 2.0
        )),
        '▐' => Some(format!(
            r#"<rect x="{x_half}" y="0" width="{x_half}" height="{cell_h}" stroke="none"/>"#,
            x_half = cell_w / 2.0
        )),

        // Fractional blocks (Lower)
        ' ' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - cell_h / 8.0,
            h = cell_h / 8.0
        )),
        '▂' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - cell_h / 4.0,
            h = cell_h / 4.0
        )),
        '▃' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - 3.0 * cell_h / 8.0,
            h = 3.0 * cell_h / 8.0
        )),
        '▅' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - 5.0 * cell_h / 8.0,
            h = 5.0 * cell_h / 8.0
        )),
        '▆' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - 3.0 * cell_h / 4.0,
            h = 3.0 * cell_h / 4.0
        )),
        '▇' => Some(format!(
            r#"<rect x="0" y="{y}" width="{cell_w}" height="{h}" stroke="none"/>"#,
            y = cell_h - 7.0 * cell_h / 8.0,
            h = 7.0 * cell_h / 8.0
        )),

        // Fractional blocks (Left)
        '▏' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = cell_w / 8.0
        )),
        '▎' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = cell_w / 4.0
        )),
        '▍' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = 3.0 * cell_w / 8.0
        )),
        '▋' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = 5.0 * cell_w / 8.0
        )),
        '▊' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = 3.0 * cell_w / 4.0
        )),
        '▉' => Some(format!(
            r#"<rect x="0" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            w = 7.0 * cell_w / 8.0
        )),

        // Upper fractional
        '▔' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{h}" stroke="none"/>"#,
            h = cell_h / 8.0
        )),

        // Right fractional
        '▕' => Some(format!(
            r#"<rect x="{x}" y="0" width="{w}" height="{cell_h}" stroke="none"/>"#,
            x = cell_w - cell_w / 8.0,
            w = cell_w / 8.0
        )),

        // Quadrants
        '▖' => Some(format!(
            r#"<rect x="0" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▗' => Some(format!(
            r#"<rect x="{cx}" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▘' => Some(format!(
            r#"<rect x="0" y="0" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▝' => Some(format!(
            r#"<rect x="{cx}" y="0" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▙' => Some(format!(
            r#"<rect x="0" y="0" width="{cx}" height="{cy}" stroke="none"/>
               <rect x="0" y="{cy}" width="{cell_w}" height="{cy}" stroke="none"/>"#
        )),
        '▚' => Some(format!(
            r#"<rect x="0" y="0" width="{cx}" height="{cy}" stroke="none"/>
               <rect x="{cx}" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▛' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cy}" stroke="none"/>
               <rect x="0" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▜' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cy}" stroke="none"/>
               <rect x="{cx}" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▞' => Some(format!(
            r#"<rect x="{cx}" y="0" width="{cx}" height="{cy}" stroke="none"/>
               <rect x="0" y="{cy}" width="{cx}" height="{cy}" stroke="none"/>"#
        )),
        '▟' => Some(format!(
            r#"<rect x="{cx}" y="0" width="{cx}" height="{cy}" stroke="none"/>
               <rect x="0" y="{cy}" width="{cell_w}" height="{cy}" stroke="none"/>"#
        )),

        // Shades
        '░' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cell_h}" fill-opacity="0.25" stroke="none"/>"#
        )),
        '▒' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cell_h}" fill-opacity="0.5" stroke="none"/>"#
        )),
        '▓' => Some(format!(
            r#"<rect x="0" y="0" width="{cell_w}" height="{cell_h}" fill-opacity="0.75" stroke="none"/>"#
        )),

        _ => None,
    }
}

fn render_box_drawing_shape_group(
    tspan: &TSpan,
    baseline_y: u32,
    y_animation: &Option<YAnimation>,
    total_ms: u32,
    parent_start: u32,
    parent_end: u32,
) -> Option<String> {
    let cell_w = tspan.cell_w as f32;
    let cell_h = tspan.cell_h as f32;

    // thickness scale
    let lw = (cell_w * 0.08).max(1.0);
    let hw = lw * 2.5;
    let d_off = (lw * 1.5).max(1.2);
    let r = (cell_w * 0.45).min(cell_h * 0.45);

    let mut shapes = Vec::new();
    let mut coord_idx = 0;
    for ch in tspan.text.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        if let Some(shape_xml) = get_box_drawing_path(ch, cell_w, cell_h, lw, hw, d_off, r) {
            shapes.push((coord_idx, shape_xml));
            coord_idx += 1;
        } else {
            return None; // If any character is not supported, we fall back to text rendering
        }
    }

    let is_tspan_static = is_static(tspan.start_ms, tspan.end_ms, total_ms);
    let matches_parent = tspan.start_ms <= parent_start && tspan.end_ms >= parent_end;
    let is_hidden = !is_tspan_static && !matches_parent;

    let set_str = if is_hidden {
        visibility_set(tspan.start_ms, tspan.end_ms, total_ms)
    } else {
        String::new()
    };

    let visibility_attr = if is_hidden {
        r#" visibility="hidden""#
    } else {
        ""
    };

    let mut result = String::new();
    for (i, shape_xml) in shapes {
        let cell_x = tspan.x_coords[i];
        let cell_y = baseline_y as f32 - tspan.baseline as f32;

        let anim_str = if let Some(anim) = y_animation {
            anim.to_svg_transform_animation(cell_x, tspan.baseline)
        } else {
            String::new()
        };

        let fg_hex = rgb_hex(tspan.fg);
        result.push_str(&format!(
            r#"<g transform="translate({cell_x}, {cell_y})"{visibility} fill="{fg}" stroke="{fg}">{set}{anim}{shape}</g>"#,
            cell_x = cell_x,
            cell_y = cell_y,
            visibility = visibility_attr,
            fg = fg_hex,
            set = set_str,
            anim = anim_str,
            shape = shape_xml,
        ));
    }

    Some(result)
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

    pub fn to_svg_string(
        &self,
        color_classes: &HashMap<[u8; 3], String>,
        total_ms: u32,
        default_letter_spacing: f32,
        default_fg: Option<[u8; 3]>,
    ) -> String {
        let mut text_tags = Vec::new();
        let mut current_non_box: Vec<&TSpan> = Vec::new();

        let parent_start = self.start_ms;
        let parent_end = self.end_ms;

        let flush_non_box = |non_box: &mut Vec<&TSpan>, tags: &mut Vec<String>| {
            if non_box.is_empty() {
                return;
            }
            let mut s = String::new();
            s.push_str(&format!(r#"<text y="{}">"#, self.y));
            if let Some(ref anim) = self.y_animation {
                s.push_str(&anim.to_svg_string());
            }
            for tspan in non_box.iter() {
                s.push_str(&tspan.to_svg_string(
                    color_classes,
                    parent_start,
                    parent_end,
                    total_ms,
                    default_letter_spacing,
                    default_fg,
                    false,
                ));
            }
            s.push_str("</text>");
            tags.push(s);
            non_box.clear();
        };

        for tspan in &self.tspans {
            if tspan.is_box {
                flush_non_box(&mut current_non_box, &mut text_tags);

                let shapes_opt = render_box_drawing_shape_group(
                    tspan,
                    self.y,
                    &self.y_animation,
                    total_ms,
                    parent_start,
                    parent_end,
                );
                if let Some(ref shapes_svg) = shapes_opt {
                    text_tags.push(shapes_svg.clone());
                }

                let text_length = if let (Some(&first_x), Some(&last_x)) =
                    (tspan.x_coords.first(), tspan.x_coords.last())
                {
                    let val = (last_x - first_x) + tspan.cell_w as f32;
                    let formatted = format!("{:.2}", val);
                    let mut trimmed = formatted.trim_end_matches('0');
                    if trimmed.ends_with('.') {
                        trimmed = &trimmed[..trimmed.len() - 1];
                    }
                    trimmed.to_string()
                } else {
                    tspan.cell_w.to_string()
                };
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

                let mut s = String::new();
                s.push_str(&format!(
                    r#"<text x="{x}" y="{y}"{transform} textLength="{text_length}" lengthAdjust="spacingAndGlyphs">"#,
                    x = tspan.x_coords[0],
                    y = y,
                    transform = transform,
                    text_length = text_length,
                ));
                if let Some(ref anim) = self.y_animation {
                    s.push_str(&anim.to_svg_string());
                }
                s.push_str(&tspan.to_svg_string(
                    color_classes,
                    parent_start,
                    parent_end,
                    total_ms,
                    default_letter_spacing,
                    default_fg,
                    shapes_opt.is_some(),
                ));
                s.push_str("</text>");
                text_tags.push(s);
            } else {
                current_non_box.push(tspan);
            }
        }
        flush_non_box(&mut current_non_box, &mut text_tags);

        if is_static(self.start_ms, self.end_ms, total_ms) {
            text_tags.join("")
        } else {
            if text_tags.len() == 1 {
                let tag = &text_tags[0];
                if let Some(idx) = tag.find('>') {
                    let mut opt = String::new();
                    opt.push_str(&tag[..idx]);
                    opt.push_str(r#" class="h""#);
                    opt.push_str(">");
                    let set_str = visibility_set(self.start_ms, self.end_ms, total_ms);
                    opt.push_str(&set_str);
                    opt.push_str(&tag[idx + 1..]);
                    opt
                } else {
                    tag.clone()
                }
            } else {
                format!(
                    r#"<g class="h">{}{}</g>"#,
                    visibility_set(self.start_ms, self.end_ms, total_ms),
                    text_tags.join(""),
                )
            }
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
    pub y_animation: Option<YAnimation>,
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
    pub letter_spacing: f32,
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
    start_ms <= 40 && (end_ms >= total_ms || total_ms.saturating_sub(end_ms) <= 200)
}

fn format_seconds(s: f32) -> String {
    let formatted = format!("{:.3}", s);
    let mut trimmed = formatted.trim_end_matches('0');
    if trimmed.ends_with('.') {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    trimmed.to_string()
}

fn format_key_time(s: f32) -> String {
    let formatted = format!("{:.4}", s);
    let mut trimmed = formatted.trim_end_matches('0');
    if trimmed.ends_with('.') {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn format_time(ms: u32) -> String {
    format_seconds(ms as f32 / 1000.0)
}

fn format_begin(ms: u32) -> String {
    if ms <= 40 {
        "t.begin".to_string()
    } else {
        format!("t.begin+{}", format_time(ms))
    }
}

fn visibility_set(start_ms: u32, end_ms: u32, total_ms: u32) -> String {
    let end = if end_ms >= total_ms {
        "t.end".to_string()
    } else {
        format!("t.begin+{}", format_time(end_ms))
    };

    format!(
        r#"<set attributeName="visibility" to="visible" begin="{}" end="{}"/>"#,
        format_begin(start_ms),
        end
    )
}

fn simplify_discrete_animation<T: PartialEq + Clone>(
    key_times: &[f32],
    values: &[T],
) -> (Vec<f32>, Vec<T>) {
    if key_times.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut simplified_kt = vec![key_times[0]];
    let mut simplified_vals = vec![values[0].clone()];
    for i in 1..values.len() {
        if values[i] != values[i - 1] {
            simplified_kt.push(key_times[i]);
            simplified_vals.push(values[i].clone());
        }
    }
    (simplified_kt, simplified_vals)
}

fn format_key_time_list(kt: &[f32]) -> String {
    kt.iter()
        .map(|&k| format_key_time(k))
        .collect::<Vec<_>>()
        .join(";")
}

fn format_val_list<T: std::fmt::Display>(vals: &[T]) -> String {
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(";")
}

fn serialize_cursor_rects(cursors: &[CursorRect], total_ms: u32) -> String {
    if cursors.is_empty() {
        return String::new();
    }

    let mut groups: HashMap<(u32, u32, [u8; 3]), Vec<CursorRect>> = HashMap::new();
    for c in cursors {
        groups
            .entry((c.w, c.h, c.fill))
            .or_default()
            .push(c.clone());
    }

    let mut s = String::new();
    for ((w, h, fill), mut list) in groups {
        list.sort_by_key(|c| c.start_ms);

        let mut boundaries = vec![0, total_ms];
        for c in &list {
            boundaries.push(c.start_ms);
            boundaries.push(c.end_ms);
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut key_times_f32 = Vec::new();
        let mut x_vals = Vec::new();
        let mut y_vals = Vec::new();
        let mut vis_vals = Vec::new();

        let mut last_x = list.first().map(|c| c.x).unwrap_or(0);
        let mut last_y = list.first().map(|c| c.y).unwrap_or(0);

        for i in 0..(boundaries.len() - 1) {
            let start = boundaries[i];
            let end = boundaries[i + 1];
            let k = start as f32 / total_ms as f32;
            key_times_f32.push(k);

            let active = list.iter().find(|c| c.start_ms <= start && c.end_ms >= end);
            if let Some(c) = active {
                x_vals.push(c.x);
                y_vals.push(c.y);
                vis_vals.push("visible");
                last_x = c.x;
                last_y = c.y;
            } else {
                x_vals.push(last_x);
                y_vals.push(last_y);
                vis_vals.push("hidden");
            }
        }

        if key_times_f32.len() == 1 && vis_vals[0] == "visible" {
            s.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{c}" fill-opacity="0.7"/>"#,
                x = x_vals[0],
                y = y_vals[0],
                w = w,
                h = h,
                c = rgb_hex(fill),
            ));
        } else {
            let mut x_attr = String::new();
            let mut y_attr = String::new();
            let mut class_attr = r#" class="h""#.to_string();

            let mut animates = Vec::new();

            let (kt_x, vals_x) = simplify_discrete_animation(&key_times_f32, &x_vals);
            if vals_x.len() == 1 {
                x_attr = format!(r#" x="{}""#, vals_x[0]);
            } else {
                let kt_str = format_key_time_list(&kt_x);
                let val_str = format_val_list(&vals_x);
                animates.push(format!(
                    r#"  <animate attributeName="x" calcMode="discrete" values="{val_str}" keyTimes="{kt_str}" dur="{dur}" begin="t.begin" fill="freeze"/>"#,
                    val_str = val_str,
                    kt_str = kt_str,
                    dur = format_time(total_ms)
                ));
            }

            let (kt_y, vals_y) = simplify_discrete_animation(&key_times_f32, &y_vals);
            if vals_y.len() == 1 {
                y_attr = format!(r#" y="{}""#, vals_y[0]);
            } else {
                let kt_str = format_key_time_list(&kt_y);
                let val_str = format_val_list(&vals_y);
                animates.push(format!(
                    r#"  <animate attributeName="y" calcMode="discrete" values="{val_str}" keyTimes="{kt_str}" dur="{dur}" begin="t.begin" fill="freeze"/>"#,
                    val_str = val_str,
                    kt_str = kt_str,
                    dur = format_time(total_ms)
                ));
            }

            let (kt_vis, vals_vis) = simplify_discrete_animation(&key_times_f32, &vis_vals);
            if vals_vis.len() == 1 {
                if vals_vis[0] == "visible" {
                    class_attr = String::new();
                }
            } else {
                let kt_str = format_key_time_list(&kt_vis);
                let val_str = format_val_list(&vals_vis);
                animates.push(format!(
                    r#"  <animate attributeName="visibility" calcMode="discrete" values="{val_str}" keyTimes="{kt_str}" dur="{dur}" begin="t.begin" fill="freeze"/>"#,
                    val_str = val_str,
                    kt_str = kt_str,
                    dur = format_time(total_ms)
                ));
            }

            if animates.is_empty() {
                s.push_str(&format!(
                    r#"<rect{x_attr}{y_attr} width="{w}" height="{h}" fill="{c}" fill-opacity="0.7"{class_attr}/>"#,
                    x_attr = x_attr,
                    y_attr = y_attr,
                    w = w,
                    h = h,
                    c = rgb_hex(fill),
                    class_attr = class_attr,
                ));
            } else {
                s.push_str(&format!(
                    r#"<rect{x_attr}{y_attr} width="{w}" height="{h}" fill="{c}" fill-opacity="0.7"{class_attr}>
{animates}
</rect>
"#,
                    x_attr = x_attr,
                    y_attr = y_attr,
                    w = w,
                    h = h,
                    c = rgb_hex(fill),
                    class_attr = class_attr,
                    animates = animates.join("\n"),
                ));
            }
        }
    }
    s
}

impl SvgDoc {
    pub fn to_svg(&self) -> String {
        // 1. Gather color classes
        let mut text_color_counts: HashMap<[u8; 3], usize> = HashMap::new();
        for te in &self.text_elements {
            for tspan in &te.tspans {
                *text_color_counts.entry(tspan.fg).or_default() += 1;
            }
        }
        let most_common_text_color = text_color_counts
            .iter()
            .max_by_key(|&(_, count)| count)
            .map(|(&color, _)| color);

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
            if Some(color) == most_common_text_color {
                continue;
            }
            if color_counts[&color] >= 5 {
                color_classes.insert(color, format!("c{}", class_id));
                class_id += 1;
            }
        }

        // 2. Assemble style block
        let mut style = self.style_block.clone();
        let mut extra_css = String::new();
        extra_css.push_str(".h { visibility: hidden; }\n");
        extra_css.push_str(".b { font-weight: bold; }\n");
        extra_css.push_str(".i { font-style: italic; }\n");
        extra_css.push_str(".u { text-decoration: underline; }\n");
        extra_css.push_str(".s { text-decoration: line-through; }\n");
        extra_css.push_str(".us { text-decoration: underline line-through; }\n");
        extra_css.push_str("text, tspan { font-kerning: none; font-variant-ligatures: none; text-rendering: geometricPrecision; }\n");
        let letter_spacing_style = if self.letter_spacing != 0.0 {
            format!(
                "letter-spacing: {}px; ",
                format_seconds(self.letter_spacing)
            )
        } else {
            String::new()
        };
        let fill_style = if let Some(default_fg) = most_common_text_color {
            format!("fill: {}; ", rgb_hex(default_fg))
        } else {
            String::new()
        };
        if !letter_spacing_style.is_empty() || !fill_style.is_empty() {
            extra_css.push_str(&format!(
                "text, tspan {{ {}{} }}\n",
                letter_spacing_style, fill_style
            ));
        }
        let mut sorted_entries: Vec<_> = color_classes.iter().collect();
        sorted_entries.sort_by_key(|(_, class_name)| class_name[1..].parse::<usize>().unwrap_or(0));
        for (color, class_name) in sorted_entries {
            extra_css.push_str(&format!(
                ".{} {{ fill: {}; }}\n",
                class_name,
                rgb_hex(*color)
            ));
        }

        if style.ends_with("</style>\n") {
            style.truncate(style.len() - 10);
            style.push_str(&extra_css);
            style.push_str("</style>\n");
        } else {
            style.push_str("<style>\n");
            style.push_str(&extra_css);
            style.push_str("</style>\n");
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

        // Canvas/Frame background optimization
        let needs_canvas_bg = self.frame_clip_path.is_some()
            || self.frame_bg_x != 0
            || self.frame_bg_y != 0
            || self.frame_bg_w != self.canvas_w
            || self.frame_bg_h != self.canvas_h
            || self.frame_bg_fill != self.canvas_bg;

        if needs_canvas_bg {
            s.push_str(&format!(
                r#"<rect width="{w}" height="{h}" fill="{bg}"/>
"#,
                w = self.canvas_w,
                h = self.canvas_h,
                bg = rgb_hex(self.canvas_bg),
            ));
        }

        // Clip path
        if let Some((x, y, w, h, r)) = self.frame_clip_path {
            s.push_str(&format!(
                r#"<defs><clipPath id="frame-clip"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" ry="{r}"/></clipPath></defs>"#,
            ));
        }

        // Frame background (formatted dynamically to omit x=0 and y=0 if they are zero)
        let mut frame_bg_attrs = Vec::new();
        if self.frame_bg_x != 0 {
            frame_bg_attrs.push(format!(r#"x="{}""#, self.frame_bg_x));
        }
        if self.frame_bg_y != 0 {
            frame_bg_attrs.push(format!(r#"y="{}""#, self.frame_bg_y));
        }
        frame_bg_attrs.push(format!(r#"width="{}""#, self.frame_bg_w));
        frame_bg_attrs.push(format!(r#"height="{}""#, self.frame_bg_h));
        frame_bg_attrs.push(format!(r#"fill="{}""#, rgb_hex(self.frame_bg_fill)));
        if self.frame_clip_path.is_some() {
            frame_bg_attrs.push(r#"clip-path="url(#frame-clip)""#.to_string());
        }
        s.push_str(&format!("<rect {}/>\n", frame_bg_attrs.join(" ")));

        // Window bar circles
        for circle in &self.window_bar_circles {
            s.push_str(&circle.to_svg_string());
        }

        // Master timer - rendered directly as an animate element under the svg root
        s.push_str(&format!(
            r#"<animate id="t" attributeName="x" from="0" to="0" dur="{dur}" begin="0s;t.end"/>
"#,
            dur = format_seconds(self.master_timer_dur)
        ));

        // Background rects
        let total_ms = (self.master_timer_dur * 1000.0).round() as u32;
        for rect in &self.bg_rects {
            let is_bg_static = is_static(rect.start_ms, rect.end_ms, total_ms);
            let fill_attr = if let Some(cls) = color_classes.get(&rect.fill) {
                if !is_bg_static {
                    format!(r#" class="{} h""#, cls)
                } else {
                    format!(r#" class="{}""#, cls)
                }
            } else {
                if !is_bg_static {
                    format!(r#" fill="{}" class="h""#, rgb_hex(rect.fill))
                } else {
                    format!(r#" fill="{}""#, rgb_hex(rect.fill))
                }
            };
            let rect_clip = if let Some(ref path) = rect.clip_path {
                format!(r#" clip-path="url(#{})""#, path)
            } else {
                String::new()
            };
            if is_bg_static {
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
                let anim_str = if let Some(ref anim) = rect.y_animation {
                    anim.to_svg_string()
                } else {
                    String::new()
                };
                let set_str = visibility_set(rect.start_ms, rect.end_ms, total_ms);
                s.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{w}" height="{h}"{fill}{clip}>{set}{anim}</rect>"#,
                    x = rect.x,
                    y = rect.y,
                    w = rect.w,
                    h = rect.h,
                    fill = fill_attr,
                    clip = rect_clip,
                    set = set_str,
                    anim = anim_str,
                ));
            }
        }

        // Text elements
        for te in &self.text_elements {
            s.push_str(&te.to_svg_string(
                &color_classes,
                total_ms,
                self.letter_spacing,
                most_common_text_color,
            ));
        }

        // Cursor rects
        s.push_str(&serialize_cursor_rects(&self.cursor_rects, total_ms));

        s.push_str("\n</svg>\n");
        s
    }
}

pub fn optimize_tspans(elements: &mut [TextElement]) {
    for te in elements {
        if te.tspans.is_empty() {
            continue;
        }
        te.tspans.sort_by(|a, b| {
            a.x_coords[0]
                .partial_cmp(&b.x_coords[0])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.start_ms.cmp(&b.start_ms))
        });

        let mut merged: Vec<TSpan> = Vec::new();
        for tspan in std::mem::take(&mut te.tspans) {
            if let Some(last) = merged.last_mut() {
                // Scenario A: Horizontal merge (same timeframe, adjacent x, same styling)
                if !last.is_box
                    && !tspan.is_box
                    && last.fg == tspan.fg
                    && last.bold == tspan.bold
                    && last.italic == tspan.italic
                    && last.underline == tspan.underline
                    && last.strikethrough == tspan.strikethrough
                    && last.is_box == tspan.is_box
                    && last.scale_y == tspan.scale_y
                    && last.cell_center_y_offset == tspan.cell_center_y_offset
                    && last.char_center_y_offset == tspan.char_center_y_offset
                    && last.letter_spacing == tspan.letter_spacing
                    && last.start_ms == tspan.start_ms
                    && last.end_ms == tspan.end_ms
                    && last.style_animations.is_empty()
                    && tspan.style_animations.is_empty()
                    && !last.text.chars().any(|c| c as u32 > 0xFFFF)
                    && !tspan.text.chars().any(|c| c as u32 > 0xFFFF)
                {
                    last.x_coords.extend(&tspan.x_coords);
                    last.text.push_str(&tspan.text);
                    continue;
                }

                // Scenario B: Temporal merge (same coordinates/text, consecutive times, differing in styles/color)
                if !last.is_box
                    && !tspan.is_box
                    && last.x_coords == tspan.x_coords
                    && last.text == tspan.text
                    && last.is_box == tspan.is_box
                    && last.scale_y == tspan.scale_y
                    && last.cell_center_y_offset == tspan.cell_center_y_offset
                    && last.char_center_y_offset == tspan.char_center_y_offset
                    && last.letter_spacing == tspan.letter_spacing
                    && last.end_ms == tspan.start_ms
                {
                    if last.style_history.is_empty() {
                        last.style_history = vec![
                            StyleKeyframe {
                                start_ms: last.start_ms,
                                fg: last.fg,
                                bold: last.bold,
                                italic: last.italic,
                                underline: last.underline,
                                strikethrough: last.strikethrough,
                            },
                            StyleKeyframe {
                                start_ms: tspan.start_ms,
                                fg: tspan.fg,
                                bold: tspan.bold,
                                italic: tspan.italic,
                                underline: tspan.underline,
                                strikethrough: tspan.strikethrough,
                            },
                        ];
                    } else {
                        last.style_history.push(StyleKeyframe {
                            start_ms: tspan.start_ms,
                            fg: tspan.fg,
                            bold: tspan.bold,
                            italic: tspan.italic,
                            underline: tspan.underline,
                            strikethrough: tspan.strikethrough,
                        });
                    }
                    last.end_ms = tspan.end_ms;
                    continue;
                }
            }
            merged.push(tspan);
        }

        // Finalize all style animations
        for tspan in &mut merged {
            if !tspan.style_history.is_empty() {
                let dur_ms = tspan.end_ms - tspan.start_ms;
                let mut anims = Vec::new();

                // Fg animation
                {
                    let mut kf = Vec::new();
                    for hist in &tspan.style_history {
                        kf.push((hist.start_ms, hist.fg));
                    }
                    let mut unique_kf: Vec<(u32, [u8; 3])> = Vec::new();
                    for item in kf {
                        if let Some(last_item) = unique_kf.last() {
                            if last_item.1 != item.1 {
                                unique_kf.push(item);
                            }
                        } else {
                            unique_kf.push(item);
                        }
                    }
                    if unique_kf.len() > 1 {
                        anims.push(StyleAnimation {
                            begin_ms: tspan.start_ms,
                            dur_ms,
                            property: AnimatedProperty::Fg(unique_kf),
                        });
                    }
                }

                // FontWeight animation
                {
                    let mut kf = Vec::new();
                    for hist in &tspan.style_history {
                        kf.push((hist.start_ms, hist.bold));
                    }
                    let mut unique_kf: Vec<(u32, bool)> = Vec::new();
                    for item in kf {
                        if let Some(last_item) = unique_kf.last() {
                            if last_item.1 != item.1 {
                                unique_kf.push(item);
                            }
                        } else {
                            unique_kf.push(item);
                        }
                    }
                    if unique_kf.len() > 1 {
                        anims.push(StyleAnimation {
                            begin_ms: tspan.start_ms,
                            dur_ms,
                            property: AnimatedProperty::FontWeight(unique_kf),
                        });
                    }
                }

                // FontStyle animation
                {
                    let mut kf = Vec::new();
                    for hist in &tspan.style_history {
                        kf.push((hist.start_ms, hist.italic));
                    }
                    let mut unique_kf: Vec<(u32, bool)> = Vec::new();
                    for item in kf {
                        if let Some(last_item) = unique_kf.last() {
                            if last_item.1 != item.1 {
                                unique_kf.push(item);
                            }
                        } else {
                            unique_kf.push(item);
                        }
                    }
                    if unique_kf.len() > 1 {
                        anims.push(StyleAnimation {
                            begin_ms: tspan.start_ms,
                            dur_ms,
                            property: AnimatedProperty::FontStyle(unique_kf),
                        });
                    }
                }

                // TextDecoration animation
                {
                    let mut kf = Vec::new();
                    for hist in &tspan.style_history {
                        kf.push((hist.start_ms, (hist.underline, hist.strikethrough)));
                    }
                    let mut unique_kf: Vec<(u32, (bool, bool))> = Vec::new();
                    for item in kf {
                        if let Some(last_item) = unique_kf.last() {
                            if last_item.1 != item.1 {
                                unique_kf.push(item);
                            }
                        } else {
                            unique_kf.push(item);
                        }
                    }
                    if unique_kf.len() > 1 {
                        anims.push(StyleAnimation {
                            begin_ms: tspan.start_ms,
                            dur_ms,
                            property: AnimatedProperty::TextDecoration(unique_kf),
                        });
                    }
                }

                tspan.style_animations = anims;
            }
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
    });

    let mut merged: Vec<TextElement> = Vec::new();
    for mut te in std::mem::take(elements) {
        if let Some(last) = merged.last_mut() {
            if last.y == te.y && last.start_ms == te.start_ms && last.end_ms == te.end_ms {
                last.tspans.append(&mut te.tspans);
                continue;
            }
        }
        merged.push(te);
    }
    *elements = merged;
}

pub fn optimize_bg_rect_scroll(bg_rects: &mut Vec<BgRect>) {
    bg_rects.sort_by_key(|r| r.start_ms);

    let mut i = 0;
    while i < bg_rects.len() {
        let mut merged = false;
        for j in (i + 1)..bg_rects.len() {
            if bg_rects[i].x == bg_rects[j].x
                && bg_rects[i].w == bg_rects[j].w
                && bg_rects[i].h == bg_rects[j].h
                && bg_rects[i].fill == bg_rects[j].fill
                && bg_rects[i].clip_path == bg_rects[j].clip_path
            {
                if bg_rects[i].end_ms == bg_rects[j].start_ms {
                    let r2_y = bg_rects[j].y;
                    let r2_start = bg_rects[j].start_ms;
                    let r2_end = bg_rects[j].end_ms;

                    let r1 = &mut bg_rects[i];
                    r1.end_ms = r2_end;
                    if let Some(ref mut anim) = r1.y_animation {
                        anim.segments.push((r2_y, r2_start));
                        anim.dur_ms = r2_end - anim.begin_ms;
                    } else {
                        r1.y_animation = Some(YAnimation {
                            begin_ms: r1.start_ms,
                            segments: vec![(r1.y, r1.start_ms), (r2_y, r2_start)],
                            dur_ms: r2_end - r1.start_ms,
                        });
                    }
                    bg_rects.remove(j);
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
                    let el1_old_end = el1.end_ms;
                    el1.end_ms = el2_end;

                    // Extend end_ms of any tspans that were active at the end of the previous phase
                    for tspan in &mut el1.tspans {
                        if tspan.end_ms == el1_old_end {
                            tspan.end_ms = el2_end;
                        }
                    }

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
pub fn group_text_elements_final(elements: &mut Vec<TextElement>) {
    if elements.is_empty() {
        return;
    }
    elements.sort_by(|a, b| {
        a.y.cmp(&b.y)
            .then_with(|| match (&a.y_animation, &b.y_animation) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(ax), Some(bx)) => ax
                    .begin_ms
                    .cmp(&bx.begin_ms)
                    .then(ax.dur_ms.cmp(&bx.dur_ms))
                    .then_with(|| ax.segments.cmp(&bx.segments)),
            })
    });

    let mut merged: Vec<TextElement> = Vec::new();
    for mut te in std::mem::take(elements) {
        if let Some(last) = merged.last_mut() {
            if last.y == te.y && last.y_animation == te.y_animation {
                last.start_ms = last.start_ms.min(te.start_ms);
                last.end_ms = last.end_ms.max(te.end_ms);
                last.tspans.append(&mut te.tspans);
                continue;
            }
        }
        merged.push(te);
    }
    for te in &mut merged {
        te.tspans.sort_by(|a, b| {
            a.x_coords[0]
                .partial_cmp(&b.x_coords[0])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    *elements = merged;
}

pub fn append_newlines_to_final_tspans(elements: &mut [TextElement]) {
    for te in elements {
        if te.tspans.is_empty() {
            continue;
        }

        // Collect all unique time boundaries (start_ms and end_ms) for tspans in this TextElement.
        let mut boundaries = Vec::new();
        for tspan in &te.tspans {
            boundaries.push(tspan.start_ms);
            boundaries.push(tspan.end_ms);
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        if boundaries.len() < 2 {
            continue;
        }

        let mut final_indices = std::collections::BTreeSet::new();

        for i in 0..(boundaries.len() - 1) {
            let start = boundaries[i];
            let end = boundaries[i + 1];

            // Find all tspans active in this interval [start, end)
            let mut active_indices = Vec::new();
            for (idx, tspan) in te.tspans.iter().enumerate() {
                if tspan.start_ms <= start && tspan.end_ms >= end {
                    active_indices.push(idx);
                }
            }

            // Since tspans are sorted by x_coords[0] ascending, the last active tspan index
            // is the rightmost one.
            if let Some(&max_idx) = active_indices.last() {
                final_indices.insert(max_idx);
            }
        }

        // Append newline to all final tspans
        for idx in final_indices {
            te.tspans[idx].text.push('\n');
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
    let first_visuals = get_frame_visuals(&frames[0]);
    for visual in first_visuals {
        current.push((visual, frames[0].t_ms, frames[0].default_bg));
    }

    // Process subsequent frames.
    for frame in frames.iter().skip(1) {
        let frame_visuals = get_frame_visuals(frame);
        for idx in 0..num_cells {
            let new_visual = frame_visuals[idx].clone();
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

    // Split cell spans row-by-row to align typing transitions and avoid overlapping lifetimes.
    let mut split_cell_spans = Vec::new();
    for r in 0..rows {
        // Find spans on this row
        let mut row_spans: Vec<CellSpan> =
            cell_spans.iter().filter(|s| s.row == r).cloned().collect();
        if row_spans.is_empty() {
            continue;
        }

        // Group by end_ms
        row_spans.sort_by_key(|s| s.end_ms);

        let mut i = 0;
        while i < row_spans.len() {
            let end = row_spans[i].end_ms;
            let mut group = Vec::new();
            while i < row_spans.len() && row_spans[i].end_ms == end {
                group.push(row_spans[i].clone());
                i += 1;
            }

            // Find the max start_ms in this group
            let max_start = group.iter().map(|s| s.start_ms).max().unwrap_or(0);

            // Split the spans in this group at max_start
            for span in group {
                if span.start_ms < max_start {
                    // Split into [start_ms, max_start] and [max_start, end_ms]
                    split_cell_spans.push(CellSpan {
                        row: span.row,
                        col: span.col,
                        start_ms: span.start_ms,
                        end_ms: max_start,
                        visual: span.visual.clone(),
                        default_bg: span.default_bg,
                    });
                    split_cell_spans.push(CellSpan {
                        row: span.row,
                        col: span.col,
                        start_ms: max_start,
                        end_ms: span.end_ms,
                        visual: span.visual.clone(),
                        default_bg: span.default_bg,
                    });
                } else {
                    split_cell_spans.push(span);
                }
            }
        }
    }
    cell_spans = split_cell_spans;

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
    let mut font_family = if opts.no_system_fonts {
        const SYSTEM_FONTS: &[&str] = &[
            "ui-monospace",
            "Menlo",
            "Consolas",
            "'DejaVu Sans Mono'",
            "\"DejaVu Sans Mono\"",
            "monospace",
        ];
        opts.font_family
            .split(',')
            .map(|s| s.trim())
            .filter(|&s| !SYSTEM_FONTS.iter().any(|&sys| s.eq_ignore_ascii_case(sys)))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        opts.font_family.clone()
    };

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
            y_animation: None,
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
            start_ms: span.start_ms,
            end_ms: span.end_ms,
            style_animations: Vec::new(),
            style_history: Vec::new(),
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
    optimize_bg_rect_scroll(&mut bg_rects);
    optimize_rows(&mut text_elements);
    group_text_elements_final(&mut text_elements);
    optimize_tspans(&mut text_elements);

    // Append newline characters at the end of the final tspan block for each row/interval.
    append_newlines_to_final_tspans(&mut text_elements);

    // Clean up redundant y_animations that don't change y coordinate
    for te in &mut text_elements {
        if let Some(ref anim) = te.y_animation {
            let first_y = anim.segments[0].0;
            if anim.segments.iter().all(|(y, _)| *y == first_y) {
                te.y_animation = None;
            }
        }
    }
    for rect in &mut bg_rects {
        if let Some(ref anim) = rect.y_animation {
            let first_y = anim.segments[0].0;
            if anim.segments.iter().all(|(y, _)| *y == first_y) {
                rect.y_animation = None;
            }
        }
    }

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
        letter_spacing: letter_spacing_svg,
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
        // Text content present (now includes trailing newline).
        assert!(svg.contains(">h\n<") || svg.contains(">hi\n<"));
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
    fn test_font_subset_on_checked_in_cjk_subset() {
        let mut rec = synth_recording();
        // Insert a CJK character to trigger the Noto Sans Mono CJK JP fallback font.
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
            "Rendering SVG with CJK characters should succeed via the checked-in subset"
        );
        let svg = result.unwrap();
        assert!(svg.contains("font-family: 'Noto Sans Mono CJK JP'"));
        assert!(svg.contains("url(data:font/woff2;base64,"));
        assert!(!svg.contains(
            "Font subsetting failed for 'Noto Sans Mono CJK JP'; embedding the full font data in this SVG."
        ));
    }

    #[test]
    fn test_noto_emoji_is_embedded_when_used() {
        let mut rec = synth_recording();
        if let Frame::Key { cells, .. } = &mut rec.frames[0] {
            cells[2] = CellSnap {
                text: "🚀".into(),
                fg: [255, 255, 255],
                bg: [0, 0, 0],
                flags: 0,
            };
        }

        let svg = render_svg_to_string(&rec, &SvgOptions::default()).unwrap();
        assert!(svg.contains("font-family: 'Noto Emoji'"));
    }

    #[test]
    fn late_non_static_text_keeps_exact_end_time() {
        let te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 600,
            end_ms: 950,
            tspans: vec![TSpan {
                x_coords: vec![10.0],
                text: "loop".to_string(),
                fg: [255, 255, 255],
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
                start_ms: 600,
                end_ms: 950,
                style_animations: Vec::new(),
                style_history: Vec::new(),
            }],
        };

        let svg = te.to_svg_string(&HashMap::new(), 1000, 0.0, None);
        assert!(svg.contains(r#"begin="t.begin+0.6" end="t.begin+0.95""#));
    }

    #[test]
    fn test_no_system_fonts_validation() {
        let mut rec = synth_recording();
        let mut opts = SvgOptions::default();
        opts.no_system_fonts = true;
        let result = render_svg_to_string(&rec, &opts);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(!svg.contains("ui-monospace"));
        assert!(!svg.contains("Menlo"));

        if let Frame::Key { cells, .. } = &mut rec.frames[0] {
            cells[2] = CellSnap {
                text: "\u{10FFFF}".into(),
                fg: [255, 255, 255],
                bg: [0, 0, 0],
                flags: 0,
            };
        }
        let result_failed = render_svg_to_string(&rec, &opts);
        assert!(result_failed.is_err());
        let err_msg = result_failed.unwrap_err().to_string();
        assert!(err_msg.contains("Glyph not found in embedded fonts for character"));
    }

    #[test]
    fn test_box_drawing_svg_rendering() {
        let mut te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 0,
            end_ms: 1000,
            tspans: vec![
                TSpan {
                    x_coords: vec![100.0],
                    text: "╭".to_string(),
                    fg: [255, 255, 255],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: true,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![110.0],
                    text: "─".to_string(),
                    fg: [255, 255, 255],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: true,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![120.0],
                    text: "╮".to_string(),
                    fg: [255, 255, 255],
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    is_box: true,
                    scale_y: 1.0,
                    cell_center_y_offset: 0.0,
                    char_center_y_offset: 0.0,
                    cell_w: 10,
                    cell_h: 20,
                    baseline: 15,
                    letter_spacing: 0.0,
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
            ],
        };

        // First, verify that optimize_tspans does NOT merge box drawing characters
        optimize_tspans(std::slice::from_mut(&mut te));
        assert_eq!(te.tspans.len(), 3);
        assert_eq!(te.tspans[0].text, "╭");
        assert_eq!(te.tspans[1].text, "─");
        assert_eq!(te.tspans[2].text, "╮");

        // Next, render to SVG string and inspect it
        let svg = te.to_svg_string(&HashMap::new(), 1000, 0.0, None);

        // The SVG should contain three individual text tags, each with textLength="10" and style="fill-opacity: 0"
        assert!(
            svg.contains(r#"x="100" y="20""#)
                && svg.contains(r#"textLength="10""#)
                && svg.contains(r#"style="fill-opacity: 0""#),
            "SVG does not contain correct transparent text tag for col 0: {}",
            svg
        );
        assert!(
            svg.contains(r#"x="110" y="20""#)
                && svg.contains(r#"textLength="10""#)
                && svg.contains(r#"style="fill-opacity: 0""#),
            "SVG does not contain correct transparent text tag for col 1: {}",
            svg
        );
        assert!(
            svg.contains(r#"x="120" y="20""#)
                && svg.contains(r#"textLength="10""#)
                && svg.contains(r#"style="fill-opacity: 0""#),
            "SVG does not contain correct transparent text tag for col 2: {}",
            svg
        );

        // The SVG should also contain the vector shape groups translated to the cell coordinates
        assert!(
            svg.contains(r#"<g transform="translate(100, 5)""#)
                && svg.contains("stroke-linejoin=\"round\""),
            "SVG does not contain vector shapes for col 0: {}",
            svg
        );
        assert!(
            svg.contains(r#"<g transform="translate(110, 5)""#)
                && svg.contains("stroke-linecap=\"butt\""),
            "SVG does not contain vector shapes for col 1: {}",
            svg
        );
        assert!(
            svg.contains(r#"<g transform="translate(120, 5)""#)
                && svg.contains("stroke-linejoin=\"round\""),
            "SVG does not contain vector shapes for col 2: {}",
            svg
        );
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
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
            ],
        };
        optimize_tspans(std::slice::from_mut(&mut te));
        assert_eq!(te.tspans.len(), 1);
        assert_eq!(te.tspans[0].text, "ab");
        assert_eq!(te.tspans[0].x_coords, vec![10.0, 20.0]);
    }

    #[test]
    fn test_optimize_tspans_temporal_merge() {
        let mut te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 0,
            end_ms: 200,
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
                    start_ms: 0,
                    end_ms: 100,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![10.0],
                    text: "a".to_string(),
                    fg: [0, 255, 0],
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
            ],
        };
        optimize_tspans(std::slice::from_mut(&mut te));
        assert_eq!(te.tspans.len(), 1);
        assert_eq!(te.tspans[0].text, "a");
        assert_eq!(te.tspans[0].x_coords, vec![10.0]);
        assert_eq!(te.tspans[0].style_animations.len(), 1);
        let anim = &te.tspans[0].style_animations[0];
        assert_eq!(anim.begin_ms, 0);
        assert_eq!(anim.dur_ms, 200);
        match &anim.property {
            AnimatedProperty::Fg(kf) => {
                assert_eq!(kf, &vec![(0, [255, 0, 0]), (100, [0, 255, 0])]);
            }
            _ => panic!("Expected Fg animation"),
        }
    }

    #[test]
    fn test_optimize_tspans_skips_surrogate_pairs() {
        let mut te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 0,
            end_ms: 1000,
            tspans: vec![
                TSpan {
                    x_coords: vec![10.0],
                    text: "🯁".to_string(),
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
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![20.0],
                    text: "🯂".to_string(),
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
                    start_ms: 0,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
            ],
        };
        optimize_tspans(std::slice::from_mut(&mut te));
        // Should not be merged because they contain surrogate pairs (code points > 0xFFFF)
        assert_eq!(te.tspans.len(), 2);
        assert_eq!(te.tspans[0].text, "🯁");
        assert_eq!(te.tspans[1].text, "🯂");
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
                y_animation: None,
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
                y_animation: None,
            },
        ];
        optimize_bg_rects(&mut rects);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 10);
        assert_eq!(rects[0].w, 20);
    }

    #[test]
    fn test_optimize_bg_rect_scroll() {
        let mut rects = vec![
            BgRect {
                x: 10,
                y: 20,
                w: 10,
                h: 20,
                fill: [255, 0, 0],
                start_ms: 0,
                end_ms: 500,
                clip_path: None,
                y_animation: None,
            },
            BgRect {
                x: 10,
                y: 40,
                w: 10,
                h: 20,
                fill: [255, 0, 0],
                start_ms: 500,
                end_ms: 1000,
                clip_path: None,
                y_animation: None,
            },
        ];
        optimize_bg_rect_scroll(&mut rects);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].start_ms, 0);
        assert_eq!(rects[0].end_ms, 1000);
        let anim = rects[0].y_animation.as_ref().unwrap();
        assert_eq!(anim.begin_ms, 0);
        assert_eq!(anim.dur_ms, 1000);
        assert_eq!(anim.segments, vec![(20, 0), (40, 500)]);
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
                    start_ms: 0,
                    end_ms: 500,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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
                    start_ms: 500,
                    end_ms: 1000,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
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

    #[test]
    fn test_group_text_elements_final() {
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                }],
            },
            TextElement {
                y: 20,
                y_animation: None,
                start_ms: 300,
                end_ms: 400,
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
                    start_ms: 300,
                    end_ms: 400,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                }],
            },
        ];

        group_text_elements_final(&mut elements);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].y, 20);
        assert_eq!(elements[0].start_ms, 100);
        assert_eq!(elements[0].end_ms, 400);
        assert_eq!(elements[0].tspans.len(), 2);
        assert_eq!(elements[0].tspans[0].text, "a");
        assert_eq!(elements[0].tspans[1].text, "b");
    }

    #[test]
    fn test_append_newlines_to_final_tspans() {
        let mut te = TextElement {
            y: 20,
            y_animation: None,
            start_ms: 0,
            end_ms: 200,
            tspans: vec![
                TSpan {
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
                    start_ms: 0,
                    end_ms: 100,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![10.0],
                    text: "hello".to_string(),
                    fg: [0, 255, 0],
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
                TSpan {
                    x_coords: vec![20.0],
                    text: "world".to_string(),
                    fg: [0, 255, 0],
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
                    start_ms: 100,
                    end_ms: 200,
                    style_animations: Vec::new(),
                    style_history: Vec::new(),
                },
            ],
        };

        // Sort them just like they would be in render_from_frames
        te.tspans.sort_by(|a, b| {
            a.x_coords[0]
                .partial_cmp(&b.x_coords[0])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.start_ms.cmp(&b.start_ms))
        });

        append_newlines_to_final_tspans(std::slice::from_mut(&mut te));

        assert_eq!(te.tspans[0].text, "hello\n");
        assert_eq!(te.tspans[1].text, "hello");
        assert_eq!(te.tspans[2].text, "world\n");
    }
}
