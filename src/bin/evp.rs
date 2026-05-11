//! `evp` binary entry point. All real work lives in the library crate
//! (`evp::*`); this file is the thinnest possible CLI shim around it.

use std::{io, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell as CompletionShell, generate};
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

// We ship a musl-linked release build for max portability. musl's
// default allocator is significantly slower than glibc's on this
// crate's gif / svg / glyph-cache workloads; mimalloc closes the gap
// and outperforms glibc on most benchmarks. Library users can pick
// their own allocator — this only applies to the bundled CLI.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to the `.tape` script. Optional when `--run-test-script` is set.
    #[arg(required_unless_present_any = ["run_test_script", "command"])]
    script: Option<PathBuf>,
    /// Run the built-in demo tape embedded in the binary. Writes to
    /// `./evp-test.gif` in the current directory unless `--output` is
    /// also given. Useful for verifying an install works end-to-end
    /// without needing any external files.
    #[arg(long)]
    run_test_script: bool,
    /// Override the script's `Output` directive.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Optional path to a TTF font file. If omitted, a system monospace
    /// font is auto-discovered.
    #[arg(long)]
    font: Option<String>,
    /// Also render the intermediate Recording as JSON to this path.
    #[arg(long = "dump-json")]
    dump_json: Option<PathBuf>,
    /// Explicit log level override.
    #[arg(long, value_enum)]
    log_level: Option<LogLevel>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the bundled VHS theme preset names.
    Themes,
    /// Parse a tape and exit without running it.
    Validate { script: PathBuf },
    /// Print a shell completion script to stdout.
    Completion { shell: CompletionShell },
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

    if let Some(command) = cli.command {
        return run_subcommand(command);
    }

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

    let render_opts = evp::RenderOptions {
        font_path: cli.font.clone().or(script.settings.font_family.clone()),
        font_size: script.settings.font_size,
        frame_style: evp::FrameStyle {
            padding_px: script.settings.padding,
            margin_px: script.settings.margin,
            margin_fill: script.settings.margin_fill,
            window_bar: script.settings.window_bar,
            window_bar_size_px: script.settings.window_bar_size,
            border_radius_px: script.settings.border_radius,
        },
    };

    info!(events = script.events.len(), "script loaded");

    let mut output_paths: Vec<PathBuf> = match cli.output.clone() {
        Some(p) => vec![p],
        None => script.outputs.iter().map(PathBuf::from).collect(),
    };
    if output_paths.is_empty() {
        bail!("no Output directive and --output not given");
    }
    if let Some(path) = cli.dump_json.clone() {
        output_paths.push(path);
    }

    let mut renderers = Vec::with_capacity(output_paths.len());
    for path in &output_paths {
        renderers.push((backend_for_output(path, &render_opts)?, path.clone()));
        info!(path = %path.display(), "streaming render while recording");
    }

    let out = evp::run_and_render(&script, renderers).context("running script + streaming renders")?;
    info!(frames = out.recording.frames.len(), "recording captured");
    for path in &output_paths {
        info!(path = %path.display(), "output written");
    }
    Ok(())
}

fn backend_for_output(
    path: &std::path::Path,
    render_opts: &evp::RenderOptions,
) -> Result<evp::renderer::RendererBackend> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("gif") {
        Ok(evp::renderer::RendererBackend::Gif(render_opts.clone()))
    } else if ext.eq_ignore_ascii_case("svg") {
        Ok(evp::renderer::RendererBackend::Svg(evp::SvgOptions {
            font_size: render_opts.font_size,
            ..Default::default()
        }))
    } else if ext.eq_ignore_ascii_case("json") {
        Ok(evp::renderer::RendererBackend::Json)
    } else {
        bail!(
            "only .gif, .svg, and .json outputs are supported (got `{}`)",
            path.display()
        );
    }
}

fn run_subcommand(command: Commands) -> Result<()> {
    match command {
        Commands::Themes => {
            for name in evp::Theme::preset_names()? {
                println!("{name}");
            }
            Ok(())
        }
        Commands::Validate { script } => {
            evp::parse_script_file(&script)
                .with_context(|| format!("parsing {}", script.display()))?;
            println!("{}: ok", script.display());
            Ok(())
        }
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "evp", &mut io::stdout());
            Ok(())
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validate_subcommand() {
        let cli = Cli::try_parse_from(["evp", "validate", "demo.tape"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Validate { script }) if script == PathBuf::from("demo.tape")
        ));
    }

    #[test]
    fn parses_completion_subcommand() {
        let cli = Cli::try_parse_from(["evp", "completion", "bash"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Completion {
                shell: CompletionShell::Bash
            })
        ));
    }
}
