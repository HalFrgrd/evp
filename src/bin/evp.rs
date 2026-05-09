//! `evp` binary entry point. All real work lives in the library crate
//! (`evp::*`); this file is the thinnest possible CLI shim around it.

use std::{
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

/// Embedded demo tape used by `--run-test-script`. Source lives in
/// `examples/test.tape` so it stays in sync with the rest of the demos.
const EMBEDDED_TEST_TAPE: &str = include_str!("../../examples/test.tape");

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
    /// Path to the `.tape` script. Optional when `--run-test-script` is set.
    #[arg(required_unless_present = "run_test_script")]
    script: Option<PathBuf>,
    /// Run the built-in demo tape embedded in the binary. Writes to
    /// `./evp-test.gif` in the current directory unless `--output` is
    /// also given. Useful for verifying an install works end-to-end
    /// without needing any external files.
    #[arg(long, conflicts_with = "recording_json")]
    run_test_script: bool,
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

    // Either parse the user's script file, or use the embedded demo.
    let script = if cli.run_test_script {
        info!("running embedded test tape (--run-test-script)");
        evp::parse_script(EMBEDDED_TEST_TAPE).context("parsing embedded test tape")?
    } else {
        let path = cli.script.as_ref().expect("clap guarantees this is set");
        evp::parse_script_file(path).with_context(|| format!("parsing {}", path.display()))?
    };

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

    let render_opts = evp::RenderOptions {
        font_path: cli.font.clone().or(script.settings.font_family.clone()),
        font_size: script.settings.font_size,
        padding_px: script.settings.padding,
    };

    info!(events = script.events.len(), "script loaded");

    let ext = output_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let out = if ext.eq_ignore_ascii_case("svg") {
        let svg_opts = evp::SvgOptions {
            font_size: render_opts.font_size,
            ..Default::default()
        };
        info!(path = %output_path.display(), "streaming svg render while recording");
        let out = evp::run_and_render_svg(&script, svg_opts, output_path.clone())
            .context("running script + streaming svg")?;
        info!(frames = out.recording.frames.len(), "recording captured");
        out
    } else {
        info!(path = %output_path.display(), "streaming gif render while recording");
        let out = evp::run_and_render_gif(&script, render_opts, output_path.clone())
            .context("running script + streaming gif")?;
        info!(frames = out.recording.frames.len(), "recording captured");
        out
    };

    if let Some(path) = &cli.recording_json {
        let bytes = evp::recording_to_json(&out.recording)?;
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        info!(path = %path.display(), "recording written");
    }

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
