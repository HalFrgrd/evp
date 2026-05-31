use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const VHS_THEMES_JSON: &str = include_str!("../assets/vhs-themes.json");

const DEFAULT_BACKGROUND: &str = "#171717";
const DEFAULT_FOREGROUND: &str = "#dddddd";
const DEFAULT_BLACK: &str = "#282a2e";
const DEFAULT_BRIGHT_BLACK: &str = "#4d4d4d";
const DEFAULT_RED: &str = "#D74E6F";
const DEFAULT_BRIGHT_RED: &str = "#FE5F86";
const DEFAULT_GREEN: &str = "#31BB71";
const DEFAULT_BRIGHT_GREEN: &str = "#00D787";
const DEFAULT_YELLOW: &str = "#D3E561";
const DEFAULT_BRIGHT_YELLOW: &str = "#EBFF71";
const DEFAULT_BLUE: &str = "#8056FF";
const DEFAULT_BRIGHT_BLUE: &str = "#9B79FF";
const DEFAULT_MAGENTA: &str = "#ED61D7";
const DEFAULT_BRIGHT_MAGENTA: &str = "#FF7AEA";
const DEFAULT_CYAN: &str = "#04D7D7";
const DEFAULT_BRIGHT_CYAN: &str = "#00FEFE";
const DEFAULT_WHITE: &str = "#bfbfbf";
const DEFAULT_BRIGHT_WHITE: &str = "#e6e6e6";
const DEFAULT_WINDOW_BAR_SIZE_PX: u32 = 30;
pub const WINDOW_BAR_DOT_RADIUS_DIVISOR: u32 = 5;
pub const WINDOW_BAR_DOT_MIN_RADIUS: u32 = 5;
pub const WINDOW_BAR_DOT_MIN_GAP: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Theme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub background: String,
    pub foreground: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    pub cursor: String,
    #[serde(rename = "cursorAccent", skip_serializing_if = "Option::is_none")]
    pub cursor_accent: Option<String>,
    pub black: String,
    #[serde(rename = "brightBlack")]
    pub bright_black: String,
    pub red: String,
    #[serde(rename = "brightRed")]
    pub bright_red: String,
    pub green: String,
    #[serde(rename = "brightGreen")]
    pub bright_green: String,
    pub yellow: String,
    #[serde(rename = "brightYellow")]
    pub bright_yellow: String,
    pub blue: String,
    #[serde(rename = "brightBlue")]
    pub bright_blue: String,
    pub magenta: String,
    #[serde(rename = "brightMagenta")]
    pub bright_magenta: String,
    pub cyan: String,
    #[serde(rename = "brightCyan")]
    pub bright_cyan: String,
    pub white: String,
    #[serde(rename = "brightWhite")]
    pub bright_white: String,
}

impl Theme {
    pub fn vhs_default() -> Self {
        Self {
            name: None,
            background: DEFAULT_BACKGROUND.to_string(),
            foreground: DEFAULT_FOREGROUND.to_string(),
            selection: None,
            cursor: DEFAULT_FOREGROUND.to_string(),
            cursor_accent: Some(DEFAULT_BACKGROUND.to_string()),
            black: DEFAULT_BLACK.to_string(),
            bright_black: DEFAULT_BRIGHT_BLACK.to_string(),
            red: DEFAULT_RED.to_string(),
            bright_red: DEFAULT_BRIGHT_RED.to_string(),
            green: DEFAULT_GREEN.to_string(),
            bright_green: DEFAULT_BRIGHT_GREEN.to_string(),
            yellow: DEFAULT_YELLOW.to_string(),
            bright_yellow: DEFAULT_BRIGHT_YELLOW.to_string(),
            blue: DEFAULT_BLUE.to_string(),
            bright_blue: DEFAULT_BRIGHT_BLUE.to_string(),
            magenta: DEFAULT_MAGENTA.to_string(),
            bright_magenta: DEFAULT_BRIGHT_MAGENTA.to_string(),
            cyan: DEFAULT_CYAN.to_string(),
            bright_cyan: DEFAULT_BRIGHT_CYAN.to_string(),
            white: DEFAULT_WHITE.to_string(),
            bright_white: DEFAULT_BRIGHT_WHITE.to_string(),
        }
    }

    pub fn from_spec(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Ok(Self::vhs_default());
        }
        match spec.chars().next() {
            Some('{') => parse_theme_json(spec),
            Some(_) => find_theme(spec),
            None => Ok(Self::vhs_default()),
        }
    }

    pub fn preset_names() -> Result<Vec<String>> {
        let mut names = load_themes()?
            .into_iter()
            .filter_map(|theme| theme.name)
            .collect::<Vec<_>>();
        names.sort_by_key(|name| name.to_ascii_lowercase());
        Ok(names)
    }

    pub fn palette_rgb(&self) -> Result<[[u8; 3]; 16]> {
        Ok([
            parse_hex_color(&self.black)?,
            parse_hex_color(&self.red)?,
            parse_hex_color(&self.green)?,
            parse_hex_color(&self.yellow)?,
            parse_hex_color(&self.blue)?,
            parse_hex_color(&self.magenta)?,
            parse_hex_color(&self.cyan)?,
            parse_hex_color(&self.white)?,
            parse_hex_color(&self.bright_black)?,
            parse_hex_color(&self.bright_red)?,
            parse_hex_color(&self.bright_green)?,
            parse_hex_color(&self.bright_yellow)?,
            parse_hex_color(&self.bright_blue)?,
            parse_hex_color(&self.bright_magenta)?,
            parse_hex_color(&self.bright_cyan)?,
            parse_hex_color(&self.bright_white)?,
        ])
    }

    pub fn background_rgb(&self) -> Result<[u8; 3]> {
        parse_hex_color(&self.background)
    }

    pub fn foreground_rgb(&self) -> Result<[u8; 3]> {
        parse_hex_color(&self.foreground)
    }

    pub fn cursor_rgb(&self) -> Result<[u8; 3]> {
        parse_hex_color(&self.cursor)
    }

    pub fn cursor_accent_rgb(&self) -> Result<Option<[u8; 3]>> {
        self.cursor_accent.as_ref().map(|s| parse_hex_color(s)).transpose()
    }

    pub fn selection_rgb(&self) -> Result<Option<[u8; 3]>> {
        self.selection.as_ref().map(|s| parse_hex_color(s)).transpose()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum WindowBarStyle {
    #[default]
    None,
    Colorful,
    ColorfulRight,
    Rings,
    RingsRight,
}

impl WindowBarStyle {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "" | "None" => Ok(Self::None),
            "Colorful" => Ok(Self::Colorful),
            "ColorfulRight" => Ok(Self::ColorfulRight),
            "Rings" => Ok(Self::Rings),
            "RingsRight" => Ok(Self::RingsRight),
            other => bail!("{other} is not a valid window bar style"),
        }
    }

    pub fn enabled(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn align_right(self) -> bool {
        matches!(self, Self::ColorfulRight | Self::RingsRight)
    }

    pub fn outlined(self) -> bool {
        matches!(self, Self::Rings | Self::RingsRight)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameStyle {
    pub canvas_width_px: Option<u32>,
    pub canvas_height_px: Option<u32>,
    pub padding_px: u32,
    pub margin_px: u32,
    pub margin_fill: [u8; 3],
    pub window_bar: WindowBarStyle,
    pub window_bar_size_px: u32,
    pub border_radius_px: u32,
}

impl Default for FrameStyle {
    fn default() -> Self {
        Self {
            canvas_width_px: None,
            canvas_height_px: None,
            padding_px: 60,
            margin_px: 0,
            margin_fill: parse_hex_color(DEFAULT_BACKGROUND)
                .expect("failed to parse hardcoded DEFAULT_BACKGROUND color"),
            window_bar: WindowBarStyle::None,
            window_bar_size_px: DEFAULT_WINDOW_BAR_SIZE_PX,
            border_radius_px: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ThemePatch {
    name: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    selection: Option<String>,
    cursor: Option<String>,
    #[serde(rename = "cursorAccent")]
    cursor_accent: Option<String>,
    black: Option<String>,
    #[serde(rename = "brightBlack")]
    bright_black: Option<String>,
    red: Option<String>,
    #[serde(rename = "brightRed")]
    bright_red: Option<String>,
    green: Option<String>,
    #[serde(rename = "brightGreen")]
    bright_green: Option<String>,
    yellow: Option<String>,
    #[serde(rename = "brightYellow")]
    bright_yellow: Option<String>,
    blue: Option<String>,
    #[serde(rename = "brightBlue")]
    bright_blue: Option<String>,
    magenta: Option<String>,
    #[serde(rename = "brightMagenta")]
    bright_magenta: Option<String>,
    cyan: Option<String>,
    #[serde(rename = "brightCyan")]
    bright_cyan: Option<String>,
    white: Option<String>,
    #[serde(rename = "brightWhite")]
    bright_white: Option<String>,
}

impl ThemePatch {
    fn resolve(self) -> Result<Theme> {
        let default = Theme::vhs_default();
        let theme = Theme {
            name: self.name.or(default.name),
            background: self.background.unwrap_or(default.background),
            foreground: self.foreground.unwrap_or(default.foreground),
            selection: self.selection.or(default.selection),
            cursor: self.cursor.unwrap_or(default.cursor),
            cursor_accent: self.cursor_accent.or(default.cursor_accent),
            black: self.black.unwrap_or(default.black),
            bright_black: self.bright_black.unwrap_or(default.bright_black),
            red: self.red.unwrap_or(default.red),
            bright_red: self.bright_red.unwrap_or(default.bright_red),
            green: self.green.unwrap_or(default.green),
            bright_green: self.bright_green.unwrap_or(default.bright_green),
            yellow: self.yellow.unwrap_or(default.yellow),
            bright_yellow: self.bright_yellow.unwrap_or(default.bright_yellow),
            blue: self.blue.unwrap_or(default.blue),
            bright_blue: self.bright_blue.unwrap_or(default.bright_blue),
            magenta: self.magenta.unwrap_or(default.magenta),
            bright_magenta: self.bright_magenta.unwrap_or(default.bright_magenta),
            cyan: self.cyan.unwrap_or(default.cyan),
            bright_cyan: self.bright_cyan.unwrap_or(default.bright_cyan),
            white: self.white.unwrap_or(default.white),
            bright_white: self.bright_white.unwrap_or(default.bright_white),
        };
        theme.palette_rgb()?;
        theme.background_rgb()?;
        theme.foreground_rgb()?;
        theme.cursor_rgb()?;
        if let Some(selection) = &theme.selection {
            parse_hex_color(selection)?;
        }
        if let Some(cursor_accent) = &theme.cursor_accent {
            parse_hex_color(cursor_accent)?;
        }
        Ok(theme)
    }
}

fn parse_theme_json(spec: &str) -> Result<Theme> {
    let patch: ThemePatch =
        serde_json::from_str(spec).with_context(|| format!("invalid `Set Theme {spec}`"))?;
    patch.resolve()
}

fn load_themes() -> Result<Vec<Theme>> {
    let patches: Vec<ThemePatch> =
        serde_json::from_str(VHS_THEMES_JSON).context("loading bundled VHS themes")?;
    patches.into_iter().map(ThemePatch::resolve).collect()
}

fn find_theme(name: &str) -> Result<Theme> {
    let themes = load_themes()?;
    if let Some(theme) = themes
        .iter()
        .find(|theme| theme.name.as_deref() == Some(name))
        .cloned()
    {
        return Ok(theme);
    }
    if let Some(theme) = themes
        .iter()
        .find(|theme| {
            theme
                .name
                .as_deref()
                .is_some_and(|theme_name| theme_name.eq_ignore_ascii_case(name))
        })
        .cloned()
    {
        return Ok(theme);
    }
    let suggestions = Theme::preset_names()?
        .into_iter()
        .filter(|candidate| {
            let candidate = candidate.to_ascii_lowercase();
            let needle = name.to_ascii_lowercase();
            candidate.starts_with(&needle) || candidate.contains(&needle)
        })
        .take(5)
        .collect::<Vec<_>>();
    if suggestions.is_empty() {
        bail!("invalid `Set Theme {name}`: theme does not exist");
    }
    Err(anyhow!(
        "invalid `Set Theme {name}`: did you mean {}",
        suggestions.join(", ")
    ))
}

pub fn parse_hex_color(value: &str) -> Result<[u8; 3]> {
    let value = value.trim();
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| anyhow!("`{value}` is not a valid color"))?;
    if hex.len() != 6 {
        bail!("`{value}` is not a valid color");
    }
    let r = u8::from_str_radix(&hex[0..2], 16)
        .with_context(|| format!("`{value}` is not a valid color"))?;
    let g = u8::from_str_radix(&hex[2..4], 16)
        .with_context(|| format!("`{value}` is not a valid color"))?;
    let b = u8::from_str_radix(&hex[4..6], 16)
        .with_context(|| format!("`{value}` is not a valid color"))?;
    Ok([r, g, b])
}

pub fn rgb_hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

pub fn window_bar_dot_metrics(bar_h: u32) -> (u32, u32) {
    let radius = (bar_h / WINDOW_BAR_DOT_RADIUS_DIVISOR).max(WINDOW_BAR_DOT_MIN_RADIUS);
    let gap = radius.max(WINDOW_BAR_DOT_MIN_GAP);
    (radius, gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_vhs_theme_presets() {
        assert!(Theme::preset_names().unwrap().len() > 300);
    }

    #[test]
    fn parses_json_theme_with_defaults() {
        let theme =
            Theme::from_spec(r##"{ "background": "#000000", "foreground": "#ffffff" }"##).unwrap();
        assert_eq!(theme.background, "#000000");
        assert_eq!(theme.foreground, "#ffffff");
        assert_eq!(theme.black, DEFAULT_BLACK);
    }

    #[test]
    fn rejects_invalid_colors() {
        let err = parse_hex_color("oops").unwrap_err();
        assert!(format!("{err:#}").contains("valid color"));
    }
}
