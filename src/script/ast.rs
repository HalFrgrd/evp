//! AST types for VHS-format `.tape` scripts.
//!
//! A script is parsed into two collections:
//!   - [`Settings`] : configuration applied before the recording starts
//!     (window geometry, font, theme, default typing speed, framerate, …).
//!   - A list of [`Event`]s : the timeline of actions executed by the runner
//!     (typing, key presses, sleeps, waits, screenshots, …).
//!
//! Time on events is left RELATIVE here (per-event duration / typing-speed
//! deltas). The runner converts the timeline to absolute timestamps once
//! settings such as `TypingSpeed` and `PlaybackSpeed` are known.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{FrameStyle, Theme, WindowBarStyle};

/// Top-level parsed script.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Script {
    /// Output destinations (`Output foo.gif`, `Output foo.svg`, etc.).
    pub outputs: Vec<String>,
    /// Aggregated `Set` directives.
    pub settings: Settings,
    /// Environment variables for the spawned shell.
    pub env: Vec<(String, String)>,
    /// Programs that must exist on `$PATH`. We don't enforce them yet but
    /// keep them around for later.
    pub require: Vec<String>,
    /// Ordered list of timeline events.
    pub events: Vec<Event>,
}

/// All `Set` directives. Defaults match vhs where possible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub shell: Option<String>,
    pub font_family: Option<String>,
    pub font_size: f32,
    pub width: Option<u32>,        // pixels
    pub height: Option<u32>,       // pixels
    pub cols: Option<u16>, // explicit override
    pub rows: Option<u16>, // explicit override
    pub padding: u32,
    pub margin: u32,
    pub margin_fill: [u8; 3],
    pub window_bar: WindowBarStyle,
    pub window_bar_size: u32,
    pub border_radius: u32,
    pub line_height: f32,
    pub letter_spacing: f32,
    pub framerate: u32,
    pub playback_speed: f32,
    pub typing_speed: Duration,
    pub theme: Theme,
    pub cursor_blink: bool,
    pub wait_timeout: Duration,
    pub wait_pattern: String,
    pub loop_offset_pct: f32,
    pub mimic_vhs: bool,
}

impl Settings {
    pub fn resolved_canvas_width(&self) -> Option<u32> {
        match self.width {
            Some(w) => Some(w),
            None => {
                if self.cols.is_some() {
                    None
                } else {
                    Some(1200)
                }
            }
        }
    }

    pub fn resolved_canvas_height(&self) -> Option<u32> {
        match self.height {
            Some(h) => Some(h),
            None => {
                if self.rows.is_some() {
                    None
                } else {
                    Some(600)
                }
            }
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        // Defaults mirror vhs's defaults from `vhs.go` / `style.go`.
        Self {
            shell: None,
            font_family: None,
            font_size: 22.0,
            width: None,
            height: None,
            cols: None,
            rows: None,
            padding: 60,
            margin: 0,
            margin_fill: FrameStyle::default().margin_fill,
            window_bar: WindowBarStyle::None,
            window_bar_size: FrameStyle::default().window_bar_size_px,
            border_radius: 0,
            line_height: 1.0,
            letter_spacing: 1.0,
            framerate: 50,
            playback_speed: 1.0,
            typing_speed: Duration::from_millis(50),
            theme: Theme::vhs_default(),
            cursor_blink: true,
            wait_timeout: Duration::from_secs(15),
            wait_pattern: ">$".to_string(),
            loop_offset_pct: 0.0,
            mimic_vhs: false,
        }
    }
}

/// A single timeline event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Type a literal string. `delay` is the per-character delay (vhs
    /// semantics: `Type@100ms "hi"` => 100ms between each `h` and `i`).
    Type { text: String, delay: Duration },
    /// Sleep for `duration` without sending input.
    Sleep(Duration),
    /// Send a single key event (press/release), optionally repeated. `delay`
    /// is the gap between successive events when `count > 1`.
    Key {
        key: KeySpec,
        action: KeyAction,
        count: u32,
        delay: Duration,
    },
    /// Wait for a regex to appear on the last line / full screen, with a
    /// per-event timeout.
    Wait {
        scope: WaitScope,
        timeout: Duration,
        pattern: String,
    },
    /// Capture a still snapshot to `path`. (Captured by the runner via the
    /// recording pipeline, then exported separately.)
    Screenshot(String),
    /// Store text in the tape-local clipboard.
    Copy(String),
    /// Paste the current tape-local clipboard contents into the PTY.
    Paste,
    /// Hide subsequent commands from the recording until [`Event::Show`].
    Hide,
    /// Resume recording after a [`Event::Hide`].
    Show,
    /// Click at coordinates.
    Click { col: u16, row: u16, delay: Duration },
    /// Right click at coordinates.
    RightClick { col: u16, row: u16, delay: Duration },
    /// Double click at coordinates.
    DoubleClick { col: u16, row: u16, delay: Duration },
    /// Click and drag mouse.
    MouseDrag {
        start_col: u16,
        start_row: u16,
        end_col: u16,
        end_row: u16,
        delay: Duration,
    },
    /// Move mouse without pressing any buttons.
    MouseMove {
        start_col: u16,
        start_row: u16,
        end_col: u16,
        end_row: u16,
        delay: Duration,
    },
    /// Scroll mouse wheel.
    MouseScroll {
        col: u16,
        row: u16,
        direction: ScrollDirection,
        delay: Duration,
    },
    /// Low-level encoded mouse input sequence.
    MouseInput {
        action: MouseAction,
        button: Option<MouseButton>,
        col: u16,
        row: u16,
    },
}

/// Scope for `Wait`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum WaitScope {
    /// Last visible line.
    Line,
    /// Full visible screen.
    Screen,
}

/// A logical key press (key + modifier set).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySpec {
    pub key: NamedKey,
    pub mods: ModSet,
}

/// All named keys vhs understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum NamedKey {
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Space,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Shift,
    Control,
    Alt,
    Super,
    /// `ScrollUp` / `ScrollDown` – we model them as keys for simplicity even
    /// though vhs implements them as multi-frame scrolls. The runner can
    /// remap these to mouse wheel events later.
    ScrollUp,
    ScrollDown,
    /// A single literal character (e.g. the `c` in `Ctrl+C`).
    Char(char),
}

/// Modifier set for a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModSet {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl ModSet {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        super_key: false,
    };

    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.super_key
    }
}

/// Action for a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAction {
    Press,
    Release,
}

/// Direction for scroll wheel event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollDirection {
    Up,
    Down,
}

/// Action for a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Identity of mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    WheelUp,
    WheelDown,
}
