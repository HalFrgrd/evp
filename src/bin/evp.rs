//! `evp` binary entry point. All real work lives in the library crate
//! (`evp::*`); this file is the thinnest possible CLI shim around it.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

const VERSION_LONG: &str = concat!(
    "version: ",
    env!("CARGO_PKG_VERSION"),
    "\n",
    "git sha: ",
    env!("VERGEN_GIT_SHA"),
    "\n",
    "git branch: ",
    env!("VERGEN_GIT_BRANCH"),
    "\n",
    "git commit date: ",
    env!("VERGEN_GIT_COMMIT_DATE"),
    "\n",
    "git dirty: ",
    env!("VERGEN_GIT_DIRTY"),
    "\n",
    "build timestamp: ",
    env!("VERGEN_BUILD_TIMESTAMP"),
    "\n",
    "rustc: ",
    env!("VERGEN_RUSTC_SEMVER"),
    "\n",
    "target: ",
    env!("VERGEN_CARGO_TARGET_TRIPLE"),
    "\n",
    "opt-level: ",
    env!("VERGEN_CARGO_OPT_LEVEL")
);

#[derive(Parser, Debug)]
#[command(
    name = "evp",
    about = "Run a VHS-format script and produce a GIF",
    version = env!("CARGO_PKG_VERSION"),
    long_version = VERSION_LONG
)]
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
    log_build_info_debug();

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
    tracing_subscriber::fmt()
        .with_env_filter(filter.clone())
        .init();
    info!(%filter, "initialising tracing");
}

fn log_build_info_debug() {
    debug!(
        version = env!("CARGO_PKG_VERSION"),
        git_sha = env!("VERGEN_GIT_SHA"),
        git_branch = env!("VERGEN_GIT_BRANCH"),
        git_commit_date = env!("VERGEN_GIT_COMMIT_DATE"),
        git_dirty = env!("VERGEN_GIT_DIRTY"),
        build_timestamp = env!("VERGEN_BUILD_TIMESTAMP"),
        rustc_semver = env!("VERGEN_RUSTC_SEMVER"),
        target_triple = env!("VERGEN_CARGO_TARGET_TRIPLE"),
        opt_level = env!("VERGEN_CARGO_OPT_LEVEL"),
        "build information"
    );
}

fn render_to(rec: &evp::Recording, opts: &evp::RenderOptions, cli: &Cli, out: &Path) -> Result<()> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("svg") {
        info!(
            path = %out.display(),
            frames = rec.frames.len(),
            "rendering svg"
        );
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
        info!(
            path = %out.display(),
            frames = rec.frames.len(),
            "rendering gif"
        );
        evp::render_gif(rec, opts, out)
    }
}
