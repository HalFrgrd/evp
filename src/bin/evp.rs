//! `evp` binary entry point. All real work lives in the library crate
//! (`evp::*`); this file is the thinnest possible CLI shim around it.

use std::{
    io,
    path::PathBuf,
    process::ExitCode,
    time::{Instant, SystemTime},
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell as CompletionShell, generate};
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
    long_version = VERSION_LONG,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to the `.tape` script. Optional when `--run-test-script` is set.
    #[arg(required_unless_present = "run_test_script")]
    script: Option<PathBuf>,
    /// Run the built-in demo tape embedded in the binary. Writes to
    /// `./evp-test.gif` in the current directory unless `--output` is
    /// also given. Useful for verifying an install works end-to-end
    /// without needing any external files.
    #[arg(long)]
    run_test_script: bool,
    /// Override the script's `Output` directives. Repeat to write multiple
    /// outputs in one run (for example `--output out.gif --output out.svg`).
    #[arg(short, long)]
    output: Vec<PathBuf>,
    /// Also render the intermediate Recording as JSON to this path.
    #[arg(long = "dump-json")]
    dump_json: Option<PathBuf>,
    /// Do not embed base64 font data inside the generated SVG output.
    #[arg(long = "no-embed-fonts")]
    no_embed_fonts: bool,
    /// Do not use system fallback fonts. Fail if any rendered glyph is missing from the loaded/embedded fonts.
    #[arg(long = "no-system-fonts")]
    no_system_fonts: bool,
    /// Mimic VHS behavior (only allow a single word for the shell, use VHS default shell options and prompt colors).
    #[arg(long = "mimic-vhs")]
    mimic_vhs: bool,
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
    run_cli(cli)
}

fn run_cli(cli: Cli) -> Result<()> {
    let evp_start = Instant::now();

    if let Some(command) = cli.command {
        return run_subcommand(command);
    }

    init_tracing(&cli, evp_start);
    log_build_info_debug();

    if cli.run_test_script {
        info!("running embedded test tape (--run-test-script)");
        let script = evp::parse_script(EMBEDDED_TEST_TAPE).context("parsing embedded test tape")?;
        run_script(&cli, &script, evp_start)?;
    } else {
        let path = cli.script.as_ref().expect("clap guarantees this is set");
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        match evp::recording_from_json(&bytes) {
            Ok(rec) => {
                info!(
                    frames = rec.frames.len(),
                    cols = rec.cols,
                    rows = rec.rows,
                    "loaded JSON recording"
                );

                let output_paths = if cli.output.is_empty() {
                    bail!("--output is required when rendering from a JSON recording");
                } else {
                    cli.output.clone()
                };

                let font_size = if rec.font_size_px > 0.0 {
                    rec.font_size_px
                } else {
                    16.0
                };
                let render_opts = evp::RenderOptions {
                    font_path: None,
                    font_size,
                    line_height: 1.0,
                    letter_spacing: rec.letter_spacing,
                    frame_style: rec.frame_style.clone(),
                    no_system_fonts: cli.no_system_fonts,
                };

                for path in &output_paths {
                    let backend = backend_for_output(
                        path,
                        &render_opts,
                        !cli.no_embed_fonts,
                        cli.no_system_fonts,
                    )?;
                    info!(path = %path.display(), "rendering from JSON recording");
                    evp::renderer::render_recording(&rec, backend, path.clone())
                        .with_context(|| format!("failed to render {}", path.display()))?;
                }
                info!(elapsed_ms = evp_start.elapsed().as_millis(), "evp finished");
            }
            Err(_) => {
                // Fall back to treating it as a script file.
                let script = evp::parse_script_file(path)
                    .with_context(|| format!("parsing {}", path.display()))?;
                run_script(&cli, &script, evp_start)?;
            }
        }
    }

    Ok(())
}

fn run_script(cli: &Cli, script: &evp::Script, evp_start: Instant) -> Result<()> {
    let mut script = script.clone();
    if cli.mimic_vhs {
        script.settings.mimic_vhs = true;
    }
    let render_opts = evp::RenderOptions {
        font_path: script.settings.font_family.clone(),
        font_size: script.settings.font_size,
        line_height: script.settings.line_height,
        letter_spacing: script.settings.letter_spacing,
        frame_style: evp::FrameStyle {
            canvas_width_px: Some(script.settings.width),
            canvas_height_px: Some(script.settings.height),
            padding_px: script.settings.padding,
            margin_px: script.settings.margin,
            margin_fill: script.settings.margin_fill,
            window_bar: script.settings.window_bar,
            window_bar_size_px: script.settings.window_bar_size,
            border_radius_px: script.settings.border_radius,
        },
        no_system_fonts: cli.no_system_fonts,
    };

    info!(events = script.events.len(), "script loaded");

    let mut output_paths: Vec<PathBuf> = if cli.output.is_empty() {
        script.outputs.iter().map(PathBuf::from).collect()
    } else {
        cli.output.clone()
    };
    if output_paths.is_empty() {
        bail!("no Output directive and --output not given");
    }
    if let Some(path) = cli.dump_json.clone() {
        output_paths.push(path);
    }

    let mut renderers = Vec::with_capacity(output_paths.len());
    for path in &output_paths {
        renderers.push((
            backend_for_output(path, &render_opts, !cli.no_embed_fonts, cli.no_system_fonts)?,
            path.clone(),
        ));
        info!(path = %path.display(), "streaming render while recording");
    }

    let stats =
        evp::run_and_render(&script, renderers).context("running script + streaming renders")?;
    info!(frames = stats.captured_frames, "frames captured");
    info!(elapsed_ms = evp_start.elapsed().as_millis(), "evp finished");
    Ok(())
}

fn backend_for_output(
    path: &std::path::Path,
    render_opts: &evp::RenderOptions,
    embed_fonts: bool,
    no_system_fonts: bool,
) -> Result<evp::renderer::RendererBackend> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("gif") {
        let mut r_opts = render_opts.clone();
        r_opts.no_system_fonts = no_system_fonts;
        Ok(evp::renderer::RendererBackend::Gif(r_opts))
    } else if ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz") {
        Ok(evp::renderer::RendererBackend::Svg(evp::SvgOptions {
            font_path: render_opts.font_path.clone(),
            font_size: render_opts.font_size,
            embed_fonts,
            no_system_fonts,
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

/// Custom timer that emits the uptime in milliseconds (down to
/// microseconds) since the program started.
struct Uptime(Instant);

impl tracing_subscriber::fmt::time::FormatTime for Uptime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let elapsed = self.0.elapsed();
        let secs = elapsed.as_secs();
        let micros = elapsed.subsec_micros();
        write!(w, "{:03}.{:06}s", secs, micros)
    }
}

fn init_tracing(cli: &Cli, start: Instant) {
    let filter = cli
        .log_level
        .map(LogLevel::as_str)
        .map(EnvFilter::new)
        .unwrap_or_else(|| {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter.clone())
        .with_timer(Uptime(start))
        .try_init();
    info!(
        start_time = %humantime::format_rfc3339_micros(SystemTime::now()),
        %filter,
        "initialising tracing"
    );
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
    fn parses_no_system_fonts_flag() {
        let cli = Cli::try_parse_from(["evp", "demo.tape", "--no-system-fonts"]).unwrap();
        assert!(cli.no_system_fonts);
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

    #[test]
    fn parses_multiple_output_flags() {
        let cli = Cli::try_parse_from([
            "evp",
            "demo.tape",
            "--output",
            "demo.gif",
            "--output",
            "demo.svg",
        ])
        .unwrap();
        assert_eq!(
            cli.output,
            vec![PathBuf::from("demo.gif"), PathBuf::from("demo.svg")]
        );
    }

    use evp::recording::{CellSnap, Frame, Recording};
    use evp::style::FrameStyle;

    fn dummy_recording() -> Recording {
        let blank = CellSnap::blank([255, 255, 255], [0, 0, 0]);
        Recording {
            cols: 80,
            rows: 24,
            framerate: 30,
            cell_width_px: 8,
            cell_height_px: 16,
            font_size_px: 16.0,
            char_height_px: 16,
            ascent_px: 12,
            letter_spacing: 0.0,
            frame_style: FrameStyle::default(),
            frames: vec![Frame::Key {
                t_ms: 0,
                cursor: None,
                default_fg: [255, 255, 255],
                default_bg: [0, 0, 0],
                cursor_color: None,
                cursor_accent: None,
                cells: vec![blank; 80 * 24],
            }],
        }
    }

    #[test]
    fn test_json_rendering_requires_output() {
        let rec = dummy_recording();
        let json_bytes = serde_json::to_vec(&rec).unwrap();
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_rec_no_output.json");
        std::fs::write(&path, json_bytes).unwrap();

        let cli = Cli::try_parse_from(["evp", path.to_str().unwrap()]).unwrap();
        let result = run_cli(cli);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("--output is required")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_json_rendering_success() {
        let rec = dummy_recording();
        let json_bytes = serde_json::to_vec(&rec).unwrap();
        let temp_dir = std::env::temp_dir();
        let json_path = temp_dir.join("test_rec_success.json");
        std::fs::write(&json_path, json_bytes).unwrap();

        let output_path = temp_dir.join("test_output.json");

        let cli = Cli::try_parse_from([
            "evp",
            json_path.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .unwrap();

        let result = run_cli(cli);
        assert!(result.is_ok());
        assert!(output_path.exists());

        let _ = std::fs::remove_file(json_path);
        let _ = std::fs::remove_file(output_path);
    }
}
