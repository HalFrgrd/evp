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
//!   [`RunStats`] only.
//! - [`run_and_return_recording`] — run with a [`FullRecording`] raw-frame
//!   consumer and return an in-memory [`Recording`].
//! - [`run_and_render_gif`] — run and stream frames into gifski while the
//!   capture is still in progress.
//! - [`run_and_render_svg`] — run and stream frames into the animated SVG
//!   assembler while capture is in progress.
//! - [`run_and_render`] — run and stream frames into one or more renderers.
//! - [`render_gif`] — turn a [`Recording`] into an animated GIF on disk.
//! - [`render_svg`] — turn a [`Recording`] into an animated SVG on disk.
//! - [`recording_to_json`] / [`recording_from_json`] — round-trip a
//!   recording through JSON.
//!
//! The submodules ([`runner`], [`full_recording`], [`recording`], [`render_gif`],
//! [`script`], [`pty`], [`keys`]) are also `pub` for callers that need
//! finer control.

pub mod full_recording;
pub mod keys;
pub mod pty;
pub mod recording;
pub mod render_common;
pub mod render_gif;
pub mod render_json;
pub mod render_svg;
pub mod renderer;
pub mod runner;
pub mod script;
pub mod style;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use full_recording::{FullRecording, FullRecordingConfig};
pub use recording::{CellChange, CellSnap, Frame, RawFrame, Recording};
pub use render_common::RenderOptions;
pub use render_svg::SvgOptions;
pub use runner::{RunOptions, RunOutput, RunStats};
pub use script::{Event, KeySpec, ModSet, NamedKey, Script, Settings, WaitScope};
pub use style::{FrameStyle, Theme, WindowBarStyle};

/// Parse a `.tape` script source string into an AST.
///
/// `Source` directives are resolved relative to the current working
/// directory. Use [`parse_script_file`] to resolve them relative to a
/// `.tape` file on disk.
pub fn parse_script(source: &str) -> Result<Script> {
    script::parse(source)
}

/// Parse a `.tape` file from disk. `Source` directives inside the file
/// are resolved relative to the file's parent directory.
pub fn parse_script_file(path: &Path) -> Result<Script> {
    script::parse_path(path)
}

/// Run a parsed script end-to-end and return pipeline stats.
///
/// Spawns the configured shell in a PTY, drives it with the scripted
/// events, and captures frames at the script's framerate.
pub fn run(script: &Script) -> Result<RunStats> {
    runner::run(script)
}

/// Run a parsed script and return the resulting in-memory recording.
///
/// This attaches a [`FullRecording`] raw-frame consumer. The command-line
/// rendering path does not use this helper, so GIF/SVG renders do not retain
/// all frames in memory.
pub fn run_and_return_recording(script: &Script) -> Result<RunOutput> {
    let opts = runner::derive_options(&script.settings);
    let full_recording = full_recording::spawn_full_recording(FullRecordingConfig {
        cols: opts.cols,
        rows: opts.rows,
        framerate: script.settings.framerate,
        cell_width_px: opts.cell_w_px,
        cell_height_px: opts.cell_h_px,
        frame_style: opts.frame_style,
        keyframe_interval: script.settings.framerate * 5,
    });
    let stats = runner::run_with_raw_frame_consumer(script, Some(full_recording.tx.clone()))
        .context("running script with full recording consumer")?;
    let recording = full_recording
        .join()
        .context("finalising full recording consumer")?;
    Ok(RunOutput { recording, stats })
}

/// Run a parsed script while streaming GIF encoding in parallel.
///
/// This streams dense frames directly from the terminal-driving thread to the
/// renderer so gifski can encode on the fly.
pub fn run_and_render_gif(
    script: &Script,
    render_opts: RenderOptions,
    output: PathBuf,
) -> Result<RunStats> {
    run_and_render(
        script,
        vec![(renderer::RendererBackend::Gif(render_opts), output)],
    )
    .context("running script with gif stream")
}

/// Run a parsed script while streaming SVG assembly in parallel.
pub fn run_and_render_svg(
    script: &Script,
    render_opts: SvgOptions,
    output: PathBuf,
) -> Result<RunStats> {
    run_and_render(
        script,
        vec![(renderer::RendererBackend::Svg(render_opts), output)],
    )
    .context("running script with svg stream")
}

/// Run a parsed script while streaming one or more renderers in parallel.
pub fn run_and_render(
    script: &Script,
    renderers: Vec<(renderer::RendererBackend, PathBuf)>,
) -> Result<RunStats> {
    let opts = runner::derive_options(&script.settings);
    let cfg = renderer::RendererConfig {
        cols: opts.cols,
        rows: opts.rows,
        framerate: script.settings.framerate,
        cell_width_px: opts.cell_w_px,
        cell_height_px: opts.cell_h_px,
        frame_style: opts.frame_style,
    };
    let mut streams = Vec::with_capacity(renderers.len());
    for (backend, output) in renderers {
        let stream = renderer::spawn_renderer(
            renderer::RendererConfig {
                cols: cfg.cols,
                rows: cfg.rows,
                framerate: cfg.framerate,
                cell_width_px: cfg.cell_width_px,
                cell_height_px: cfg.cell_height_px,
                frame_style: cfg.frame_style,
            },
            backend,
            output,
        )
        .context("spawning renderer stream")?;
        streams.push(stream);
    }

    let raw_frame_consumers = streams.iter().map(|stream| stream.tx.clone()).collect();
    let stats = runner::run_with_raw_frame_consumers(script, raw_frame_consumers)
        .context("running script with renderer streams")?;
    for stream in streams {
        stream.join().context("finalising renderer stream")?;
    }
    Ok(stats)
}

/// Run a parsed script while streaming JSON recording output in parallel.
pub fn run_and_render_json(script: &Script, output: PathBuf) -> Result<RunStats> {
    run_and_render(script, vec![(renderer::RendererBackend::Json, output)])
        .context("running script with json stream")
}

/// Render a [`Recording`] as intermediate JSON written to `output`.
pub fn render_json(rec: &Recording, output: &Path) -> Result<()> {
    renderer::render_recording(rec, renderer::RendererBackend::Json, output.to_path_buf())
        .context("rendering json")
}

/// Render a [`Recording`] as an animated GIF written to `output`.
pub fn render_gif(rec: &Recording, opts: &RenderOptions, output: &Path) -> Result<()> {
    renderer::render_recording(
        rec,
        renderer::RendererBackend::Gif(RenderOptions {
            font_path: opts.font_path.clone(),
            font_size: opts.font_size,
            frame_style: rec.frame_style,
        }),
        output.to_path_buf(),
    )
    .context("rendering gif")
}

/// Render a [`Recording`] as an animated SVG written to `output`.
pub fn render_svg(rec: &Recording, opts: &SvgOptions, output: &Path) -> Result<()> {
    renderer::render_recording(
        rec,
        renderer::RendererBackend::Svg(opts.clone()),
        output.to_path_buf(),
    )
    .context("rendering svg")
}

/// Serialise a [`Recording`] to pretty-printed JSON bytes.
pub fn recording_to_json(rec: &Recording) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(rec).context("serialising recording")
}

/// Deserialise a [`Recording`] from JSON bytes.
pub fn recording_from_json(bytes: &[u8]) -> Result<Recording> {
    serde_json::from_slice(bytes).context("deserialising recording")
}
