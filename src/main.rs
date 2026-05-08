//! `evp` – record terminal sessions from VHS-style scripts into GIFs.

mod encoder;
mod keys;
mod pty;
mod recording;
mod render;
mod runner;
mod script;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "evp", about = "Run a VHS-format script and produce a GIF")]
struct Cli {
    /// Path to the `.tape` script.
    script: PathBuf,
    /// Override the script's `Output` directive.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Optional path to a TTF font file. If omitted, a system monospace
    /// font is auto‑discovered.
    #[arg(long)]
    font: Option<String>,
    /// Also dump the intermediate Recording as JSON to this path.
    #[arg(long)]
    recording_json: Option<PathBuf>,
}

fn main() -> ExitCode {
    if let Err(e) = real_main() {
        error!(error = ?e, "evp failed");
        eprintln!("error: {e:#}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn real_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let src = std::fs::read_to_string(&cli.script)
        .with_context(|| format!("reading {}", cli.script.display()))?;
    let script = script::parse(&src)?;

    // Resolve output path: CLI flag wins, otherwise first `Output` directive.
    let output_path: PathBuf = match cli.output.clone() {
        Some(p) => p,
        None => script
            .outputs
            .first()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("no Output directive and --output not given"))?,
    };
    if !output_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        bail!(
            "only .gif output is supported (got `{}`)",
            output_path.display()
        );
    }

    info!(events = script.events.len(), "script loaded");

    let out = runner::run(&script).context("running script")?;
    info!(frames = out.recording.frames.len(), "recording captured");

    if let Some(path) = &cli.recording_json {
        let json = serde_json::to_vec_pretty(&out.recording)?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
        info!(path = %path.display(), "recording written");
    }

    let render_opts = render::RenderOptions {
        font_path: cli.font.or(script.settings.font_family.clone()),
        font_size: script.settings.font_size,
        padding_px: script.settings.padding,
    };
    render_to(&out.recording, &render_opts, &output_path)?;
    info!(path = %output_path.display(), "gif written");
    Ok(())
}

fn render_to(rec: &recording::Recording, opts: &render::RenderOptions, out: &Path) -> Result<()> {
    render::render_gif(rec, opts, out).context("rendering gif")
}
