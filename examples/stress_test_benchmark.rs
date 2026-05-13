//! Stress-test benchmark for the evp PTY → raw-frame-consumer pipeline.
//!
//! Drives `scripts/stress_test.tape` (100×30 grid, 50 fps, 5 s, types at
//! 50 Hz) which in turn runs `scripts/stress_test_program.py`. The
//! stress_test program redraws *every cell* with random ASCII + random
//! fg/bg + random modifiers on every keystroke, producing the
//! worst-case "every cell changed" frame the renderer can be asked to
//! handle.
//!
//! The bench reports:
//!
//!   * total wall-clock time spent
//!   * frames the runner intended to capture vs. how many it captured
//!   * dropped raw-frame consumer frames
//!   * high-water mark for raw-frame consumer queues
//!
//! Exits with a non-zero status if more than 5 % of raw-frame consumer sends
//! were dropped — that's the explicit pass/fail signal the GHA
//! workflow looks at.
//!
//! Pinning to a single physical core is the caller's responsibility:
//! invoke this binary under `taskset -c 0 …` (Linux) or equivalent.
//! The benchmark records the CPU set it observes via
//! `sched_getaffinity` so the report makes the constraint visible.

use std::{fs, path::PathBuf, process::ExitCode, time::Instant};

use anyhow::{Context, Result};
use evp::{FrameStyle, RenderOptions, RunStats};

/// Hard pass/fail threshold: > 5 % dropped raw-frame consumer sends is a failure.
const MAX_DROPPED_FRACTION: f64 = 0.05;

fn main() -> ExitCode {
    // Make sure tracing output from evp ends up on stderr so the bench
    // numbers on stdout stay easy to grep.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    match run() {
        Ok(passed) => {
            if passed {
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "STRESS_TEST BENCH FAILED: dropped-consumer fraction exceeded {:.0}%",
                    MAX_DROPPED_FRACTION * 100.0
                );
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("STRESS_TEST BENCH ERROR: {e:?}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool> {
    let out_gif = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/evp-stress_test.gif"));
    let report_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/evp-stress_test-report.txt"));

    let tape_path = locate_tape()?;
    let stress_test_path = locate_stress_test_program()?;
    install_stress_test_program(&stress_test_path)?;

    let tape_src = fs::read_to_string(&tape_path)
        .with_context(|| format!("reading tape {}", tape_path.display()))?;
    let mut script = evp::parse_script(&tape_src).context("parsing stress_test tape")?;
    // Drop any `Output` directive in the tape; the binary controls the
    // output path itself.
    script.outputs.clear();

    let render_opts = crate::RenderOptions {
        font_path: None,
        font_size: script.settings.font_size,
        frame_style: FrameStyle {
            padding_px: script.settings.padding,
            margin_px: script.settings.margin,
            margin_fill: script.settings.margin_fill,
            window_bar: script.settings.window_bar,
            window_bar_size_px: script.settings.window_bar_size,
            border_radius_px: script.settings.border_radius,
        },
    };

    let cpu_set = current_cpu_affinity().unwrap_or_else(|| "(unknown)".to_string());
    eprintln!("stress_test: starting (cpu_affinity={cpu_set})");

    let started = Instant::now();
    let stats =
        evp::run_and_render_gif(&script, render_opts, out_gif.clone()).context("evp run")?;
    let wall_ms = started.elapsed().as_millis();

    let gif_bytes = fs::metadata(&out_gif).map(|m| m.len()).unwrap_or(0);

    let dropped_pct = stats.dropped_consumer_fraction() * 100.0;

    let report = format_report(&cpu_set, wall_ms, gif_bytes, &stats, dropped_pct, &out_gif);

    print!("{report}");
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&report_path, &report)
        .with_context(|| format!("writing report {}", report_path.display()))?;
    eprintln!("stress_test: report written to {}", report_path.display());

    Ok(dropped_pct <= MAX_DROPPED_FRACTION * 100.0)
}

#[allow(clippy::too_many_arguments)]
fn format_report(
    cpu_set: &str,
    wall_ms: u128,
    gif_bytes: u64,
    stats: &RunStats,
    dropped_pct: f64,
    out_gif: &PathBuf,
) -> String {
    let mut s = String::new();
    s.push_str("=== evp stress_test benchmark ===\n");
    s.push_str("renderer            = evp\n");
    s.push_str(&format!("output_gif          = {}\n", out_gif.display()));
    s.push_str(&format!("output_gif_bytes    = {gif_bytes}\n"));
    s.push_str(&format!("wall_ms             = {wall_ms}\n"));
    s.push_str(&format!("cpu_affinity        = {cpu_set}\n"));
    s.push_str(&format!(
        "expected_frames     = {}\n",
        stats.expected_frames
    ));
    s.push_str(&format!(
        "captured_frames     = {}\n",
        stats.captured_frames
    ));
    s.push_str(&format!(
        "consumer_count      = {}\n",
        stats.raw_frame_consumer_count
    ));
    s.push_str(&format!(
        "max_consumer_queue  = {} / 4096\n",
        stats.max_raw_frame_consumer_queue_len
    ));
    s.push_str(&format!(
        "dropped_consumer    = {} ({dropped_pct:.2}%)\n",
        stats.raw_frame_consumer_dropped_frames
    ));
    s.push_str(&format!(
        "dropped_threshold   = {:.0}%\n",
        MAX_DROPPED_FRACTION * 100.0
    ));
    s.push_str(&format!(
        "result              = {}\n",
        if dropped_pct <= MAX_DROPPED_FRACTION * 100.0 {
            "PASS"
        } else {
            "FAIL"
        }
    ));
    s
}

fn locate_tape() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("EVP_STRESS_TEST_TAPE") {
        return Ok(PathBuf::from(p));
    }
    // Look up from CWD a few levels.
    let candidates = [
        PathBuf::from("scripts/stress_test.tape"),
        PathBuf::from("../scripts/stress_test.tape"),
        PathBuf::from("/work/scripts/stress_test.tape"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!("couldn't find scripts/stress_test.tape; set EVP_STRESS_TEST_TAPE to its path");
}

fn locate_stress_test_program() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("EVP_STRESS_TEST_PROGRAM") {
        return Ok(PathBuf::from(p));
    }
    let candidates = [
        PathBuf::from("scripts/stress_test_program.py"),
        PathBuf::from("../scripts/stress_test_program.py"),
        PathBuf::from("/work/scripts/stress_test_program.py"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "couldn't find scripts/stress_test_program.py; set EVP_STRESS_TEST_PROGRAM to its path"
    );
}

/// The tape hard-codes `/tmp/stress_test_program.py` so the same file works
/// for both the evp run (this binary) and the VHS run (separate Docker
/// container). Copy the program into place so the spawned shell can
/// find it.
fn install_stress_test_program(src: &PathBuf) -> Result<()> {
    let dst = PathBuf::from("/tmp/stress_test_program.py");
    let bytes = fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    fs::write(&dst, &bytes).with_context(|| format!("writing {}", dst.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&dst) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&dst, perms);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_cpu_affinity() -> Option<String> {
    // /proc/self/status -> "Cpus_allowed_list:\t0-3" style line.
    let status = fs::read_to_string("/proc/self/status").ok()?;
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
