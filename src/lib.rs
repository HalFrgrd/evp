//! `evp` — record terminal sessions from VHS-style scripts.
//!
//! This crate is primarily used through its `evp` binary, but every
//! piece of the pipeline is also reachable as a library so it can be
//! embedded into other tools and exercised from integration tests
//! without spawning a subprocess.
//!
//! The high-level entry points live at the crate root:
//!
//! - [`parse_script`] — parse a `.tape` source string into a [`Script`].
//! - [`run`] — drive the script end-to-end against a real PTY, returning
//!   an in-memory [`Recording`].
//! - [`render_gif`] — turn a [`Recording`] into an animated GIF on disk.
//! - [`recording_to_json`] / [`recording_from_json`] — round-trip a
//!   recording through JSON.
//!
//! The submodules ([`runner`], [`encoder`], [`recording`], [`render`],
//! [`script`], [`pty`], [`keys`]) are also `pub` for callers that need
//! finer control.

pub mod encoder;
pub mod keys;
pub mod pty;
pub mod recording;
pub mod render;
pub mod runner;
pub mod script;

use std::path::Path;

use anyhow::{Context, Result};

pub use recording::{CellChange, CellSnap, Frame, RawFrame, Recording};
pub use render::RenderOptions;
pub use runner::{RunOptions, RunOutput};
pub use script::{Event, KeySpec, ModSet, NamedKey, Script, Settings, WaitScope};

/// Parse a `.tape` script source string into an AST.
pub fn parse_script(source: &str) -> Result<Script> {
    script::parse(source)
}

/// Run a parsed script end-to-end and return the resulting recording.
///
/// Spawns the configured shell in a PTY, drives it with the scripted
/// events, and captures frames at the script's framerate.
pub fn run(script: &Script) -> Result<RunOutput> {
    runner::run(script)
}

/// Render a [`Recording`] as an animated GIF written to `output`.
pub fn render_gif(rec: &Recording, opts: &RenderOptions, output: &Path) -> Result<()> {
    render::render_gif(rec, opts, output).context("rendering gif")
}

/// Serialise a [`Recording`] to pretty-printed JSON bytes.
pub fn recording_to_json(rec: &Recording) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(rec).context("serialising recording")
}

/// Deserialise a [`Recording`] from JSON bytes.
pub fn recording_from_json(bytes: &[u8]) -> Result<Recording> {
    serde_json::from_slice(bytes).context("deserialising recording")
}
