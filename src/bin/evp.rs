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

/// Embedded demo tape used by `run-sample-script`. Source lives in
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

fn cli_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
        .header(
            clap::builder::styling::AnsiColor::Yellow.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
        .usage(
            clap::builder::styling::AnsiColor::Yellow.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
        .literal(
            clap::builder::styling::AnsiColor::Green.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
        .placeholder(clap::builder::styling::AnsiColor::White.on_default())
        .error(
            clap::builder::styling::AnsiColor::Red.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
        .valid(
            clap::builder::styling::AnsiColor::Green.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
        .invalid(
            clap::builder::styling::AnsiColor::Red.on_default()
                | clap::builder::styling::Effects::BOLD,
        )
}

#[derive(Parser, Debug)]
#[command(
    name = "evp",
    about = "Run a VHS-format tape and produce demo GIFs or SVGs.",
    version = env!("CARGO_PKG_VERSION"),
    long_version = VERSION_LONG,
    subcommand_negates_reqs = true,
    styles = cli_styles()
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to the `.tape` tape.
    #[arg(required = true)]
    tape: Option<PathBuf>,
    /// Override the tape's `Output` directives. Repeat to write multiple
    /// outputs in one run (for example `--output out.gif --output out.svg`).
    #[arg(short, long, global = true)]
    output: Vec<PathBuf>,
    /// Do not embed base64 font data inside the generated SVG output.
    #[arg(long = "no-embed-fonts", global = true)]
    no_embed_fonts: bool,
    /// Do not use system fallback fonts. Fail if any rendered glyph is missing from the loaded/embedded fonts.
    #[arg(long = "no-system-fonts", global = true)]
    no_system_fonts: bool,
    /// Mimic VHS behavior (only allow a single word for the shell, use VHS default shell options and prompt colors).
    #[arg(long = "mimic-vhs", global = true)]
    mimic_vhs: bool,
    /// Explicit log level override.
    #[arg(long, value_enum, global = true)]
    log_level: Option<LogLevel>,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    /// Print the bundled VHS theme preset names.
    Themes,
    /// Parse a tape and exit without running it.
    Validate { tape: PathBuf },
    /// Print a shell completion script to stdout.
    Completion { shell: CompletionShell },
    /// Run the built-in demo tape embedded in the binary.
    #[command(name = "run-sample-script")]
    RunSampleScript,
    /// Print the reference tape from README.md commented out, followed by the test script.
    #[command(name = "print-ref-script")]
    PrintRefScript,
    /// Record an interactive terminal session to a tape and a GIF.
    Record {
        /// Override the columns width of the terminal.
        #[arg(long)]
        cols: Option<u16>,
        /// Override the rows height of the terminal.
        #[arg(long)]
        rows: Option<u16>,
        /// Shell program to start (e.g. bash, zsh, fish).
        #[arg(long)]
        shell: Option<String>,
        /// Predefined color theme to apply (e.g. "Catppuccin Mocha").
        #[arg(long)]
        theme: Option<String>,
    },
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
    evp::telemetry::clear_timings();
    let evp_start = Instant::now();

    if let Some(command) = cli.command.clone() {
        return run_subcommand(command, &cli, evp_start);
    }

    init_tracing(&cli, evp_start);
    log_build_info_debug();

    let path = cli.tape.as_ref().expect("clap guarantees this is set");
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
            canvas_width_px: script.settings.resolved_canvas_width(),
            canvas_height_px: script.settings.resolved_canvas_height(),
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

    let output_paths: Vec<PathBuf> = if cli.output.is_empty() {
        script.outputs.iter().map(PathBuf::from).collect()
    } else {
        cli.output.clone()
    };
    if output_paths.is_empty() {
        bail!("no Output directive and --output not given");
    }

    let mut stats_paths = Vec::new();
    let mut render_paths = Vec::new();
    for path in output_paths {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("stats") {
            stats_paths.push(path);
        } else {
            render_paths.push(path);
        }
    }

    let mut renderers = Vec::with_capacity(render_paths.len());
    for path in &render_paths {
        renderers.push((
            backend_for_output(path, &render_opts, !cli.no_embed_fonts, cli.no_system_fonts)?,
            path.clone(),
        ));
        info!(path = %path.display(), "streaming render while recording");
    }

    let stats =
        evp::run_and_render(&script, renderers).context("running script + streaming renders")?;
    info!(frames = stats.captured_frames, "frames captured");

    if !stats_paths.is_empty() {
        let cpu_set = current_cpu_affinity().unwrap_or_else(|| "(unknown)".to_string());
        let stats_out = StatsOutput {
            expected_frames: stats.expected_frames,
            captured_frames: stats.captured_frames,
            raw_frame_consumer_count: stats.raw_frame_consumer_count,
            max_raw_frame_consumer_queue_len: stats.max_raw_frame_consumer_queue_len,
            raw_frame_consumer_dropped_frames: stats.raw_frame_consumer_dropped_frames,
            wall_ms: evp_start.elapsed().as_millis(),
            total_events: script.events.len(),
            cpu_affinity: cpu_set,
            telemetry: evp::telemetry::get_timings(),
        };
        let json_str =
            serde_json::to_string_pretty(&stats_out).context("serializing run stats to JSON")?;
        for path in &stats_paths {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, &json_str)
                .with_context(|| format!("writing stats to {}", path.display()))?;
            info!(path = %path.display(), "wrote stats output");
        }
    }

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
    if ext.eq_ignore_ascii_case("stats") {
        bail!(
            "rendering `.stats` files from intermediate JSON recordings is not supported; `.stats` can only be generated when running a `.tape` script."
        );
    }
    evp::renderer::RendererBackend::for_path(path, render_opts, embed_fonts, no_system_fonts)
        .map_err(|_| {
            anyhow::anyhow!(
                "only .gif, .svg, .json, and .stats outputs are supported (got `{}`)",
                path.display()
            )
        })
}

fn run_subcommand(command: Commands, cli: &Cli, evp_start: Instant) -> Result<()> {
    match command {
        Commands::Themes => {
            for name in evp::Theme::preset_names()? {
                println!("{name}");
            }
            Ok(())
        }
        Commands::Validate { tape } => {
            evp::parse_script_file(&tape).with_context(|| format!("parsing {}", tape.display()))?;
            println!("{}: ok", tape.display());
            Ok(())
        }
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "evp", &mut io::stdout());
            Ok(())
        }
        Commands::RunSampleScript => {
            init_tracing(cli, evp_start);
            log_build_info_debug();
            info!("running embedded test tape (run-sample-script)");
            let script =
                evp::parse_script(EMBEDDED_TEST_TAPE).context("parsing embedded test tape")?;
            run_script(cli, &script, evp_start)?;
            Ok(())
        }
        Commands::PrintRefScript => {
            println!("{}", evp::script::write_reference_header().trim_end());
            println!("\n{}", EMBEDDED_TEST_TAPE);
            Ok(())
        }
        Commands::Record {
            cols,
            rows,
            shell,
            theme,
        } => {
            init_tracing(cli, evp_start);
            log_build_info_debug();

            let mut tape = PathBuf::from("demo.tape");
            let mut output_override = None;
            for out in &cli.output {
                if out.extension().map_or(false, |ext| ext == "tape") {
                    tape = out.clone();
                } else {
                    output_override = Some(out.clone());
                }
            }

            evp::record(tape, shell, cols, rows, theme, output_override)?;
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

#[derive(serde::Serialize)]
struct StatsOutput {
    expected_frames: u64,
    captured_frames: u64,
    raw_frame_consumer_count: usize,
    max_raw_frame_consumer_queue_len: usize,
    raw_frame_consumer_dropped_frames: u64,
    wall_ms: u128,
    total_events: usize,
    cpu_affinity: String,
    telemetry: std::collections::HashMap<String, u128>,
}

#[cfg(target_os = "linux")]
fn current_cpu_affinity() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Cpus_allowed_list:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn current_cpu_affinity() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validate_subcommand() {
        let cli = Cli::try_parse_from(["evp", "validate", "demo.tape"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Validate { tape }) if tape == PathBuf::from("demo.tape")
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
    fn parses_run_sample_script_subcommand() {
        let cli = Cli::try_parse_from(["evp", "run-sample-script"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::RunSampleScript)));
    }

    #[test]
    fn parses_print_ref_script_subcommand() {
        let cli = Cli::try_parse_from(["evp", "print-ref-script"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::PrintRefScript)));
    }

    #[test]
    fn parses_record_subcommand() {
        let cli = Cli::try_parse_from([
            "evp",
            "record",
            "--output",
            "my_demo.tape",
            "--cols",
            "100",
            "--rows",
            "40",
            "--shell",
            "zsh",
            "--theme",
            "Catppuccin Mocha",
        ])
        .unwrap();

        match &cli.command {
            Some(Commands::Record {
                cols,
                rows,
                shell,
                theme,
            }) => {
                assert_eq!(cli.output, vec![PathBuf::from("my_demo.tape")]);
                assert_eq!(cols, &Some(100));
                assert_eq!(rows, &Some(40));
                assert_eq!(shell, &Some("zsh".to_string()));
                assert_eq!(theme, &Some("Catppuccin Mocha".to_string()));
            }
            _ => panic!("Expected Commands::Record, got {:?}", cli.command),
        }
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
                mouse_cursor: None,
                title: None,
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
