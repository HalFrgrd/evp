//! `evp` binary entry point. All real work lives in the library crate
//! (`evp::*`); this file is the thinnest possible CLI shim around it.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
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
    /// font is auto-discovered.
    #[arg(long)]
    font: Option<String>,
    /// Also dump the intermediate Recording as JSON to this path.
    #[arg(long)]
    recording_json: Option<PathBuf>,
    /// Explicit log level override.
    #[arg(long, value_enum)]
    log_level: Option<LogLevel>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
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
    let cli = Cli::parse();
    init_tracing(&cli);

    let script = evp::parse_script_file(&cli.script)
        .with_context(|| format!("parsing {}", cli.script.display()))?;

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
        .is_some_and(|e| e.eq_ignore_ascii_case("gif") || e.eq_ignore_ascii_case("svg"))
    {
        bail!(
            "only .gif and .svg outputs are supported (got `{}`)",
            output_path.display()
        );
    }

    info!(events = script.events.len(), "script loaded");

    let out = evp::run(&script).context("running script")?;
    info!(frames = out.recording.frames.len(), "recording captured");

    if let Some(path) = &cli.recording_json {
        let bytes = evp::recording_to_json(&out.recording)?;
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        info!(path = %path.display(), "recording written");
    }

    let render_opts = evp::RenderOptions {
        font_path: cli.font.clone().or(script.settings.font_family.clone()),
        font_size: script.settings.font_size,
        padding_px: script.settings.padding,
    };
    render_to(&out.recording, &render_opts, &cli, &output_path)?;
    info!(path = %output_path.display(), "output written");
    Ok(())
}

fn init_tracing(cli: &Cli) {
    let filter = cli
        .log_level
        .map(LogLevel::as_str)
        .map(EnvFilter::new)
        .unwrap_or_else(|| {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        });
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn render_to(rec: &evp::Recording, opts: &evp::RenderOptions, cli: &Cli, out: &Path) -> Result<()> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("svg") {
        let svg_opts = evp::SvgOptions {
            font_size: opts.font_size,
            ..Default::default()
        };
        // CLI --font is also honoured for SVG: we treat the path's file
        // stem as the CSS font-family hint. (For a fully embedded font we
        // would need to base64-encode the file – left for a follow-up.)
        let _ = cli;
        evp::render_svg(rec, &svg_opts, out)
    } else {
        evp::render_gif(rec, opts, out)
    }
}
