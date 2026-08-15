use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;

use crate::util::{
    cmd::{cargo_build_release_bins, run_command},
    gif::{GifAnalysis, analyze_gif},
    paths::{evp_bin_path, project_root},
};

#[derive(Args, Debug)]
pub struct StressArgs {
    /// Path to tape script
    #[arg(long, default_value = "scripts/stress_test.tape")]
    pub tape: PathBuf,

    /// Output directory for generated artifacts and comparison report
    #[arg(long, default_value = "stress_test-out")]
    pub output_dir: PathBuf,

    /// Also run VHS via Docker and produce a head-to-head comparison
    #[arg(long)]
    pub compare_vhs: bool,

    /// Docker image for VHS comparison
    #[arg(long, default_value = "ghcr.io/charmbracelet/vhs:v0.11.0")]
    pub vhs_image: String,

    /// Optional CPU core to pin execution to (e.g. 0)
    #[arg(long, default_value = "0")]
    pub cpu_pin: String,

    /// Skip building release binaries before running
    #[arg(long)]
    pub no_build: bool,
}

pub fn run(args: StressArgs) -> Result<()> {
    let root = project_root();
    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir
    } else {
        root.join(args.output_dir)
    };
    fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    if !args.no_build {
        println!("==> Building release binaries for evp and evp_helper_tool...");
        cargo_build_release_bins(&root)?;
    }

    let evp_bin = evp_bin_path();
    if !evp_bin.exists() {
        bail!("evp binary not found at {}", evp_bin.display());
    }

    let tape_path = if args.tape.is_absolute() {
        args.tape
    } else {
        root.join(args.tape)
    };
    if !tape_path.exists() {
        bail!("tape file not found at {}", tape_path.display());
    }

    let evp_gif = out_dir.join("evp.gif");
    let evp_stats = out_dir.join("evp.stats");

    println!("==> Running evp stress test: {}", tape_path.display());
    let mut evp_cmd = if cfg!(target_os = "linux") && !args.cpu_pin.is_empty() {
        let mut c = Command::new("taskset");
        c.arg("-c").arg(&args.cpu_pin).arg(&evp_bin);
        c
    } else {
        Command::new(&evp_bin)
    };

    evp_cmd
        .current_dir(&root)
        .arg(&tape_path)
        .arg("--output")
        .arg(&evp_gif)
        .arg("--output")
        .arg(&evp_stats);

    run_command(&mut evp_cmd, "running evp stress test")?;

    let evp_stats_json: Value = if evp_stats.exists() {
        let raw = fs::read_to_string(&evp_stats)?;
        serde_json::from_str(&raw).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    let evp_analysis = analyze_gif(&evp_gif, 50, "evp")?;
    let evp_size = fs::metadata(&evp_gif).map(|m| m.len()).unwrap_or(0);

    let (vhs_gif, vhs_analysis, vhs_wall_ms, vhs_size) = if args.compare_vhs {
        println!("==> Running VHS comparison in Docker container...");
        let (gif, analysis, wall, size) =
            run_vhs_comparison(&root, &out_dir, &tape_path, &args.vhs_image, &args.cpu_pin)?;
        (Some(gif), Some(analysis), Some(wall), Some(size))
    } else {
        (None, None, None, None)
    };

    let report_path = out_dir.join("comparison.md");
    generate_report(
        &report_path,
        &evp_analysis,
        evp_size,
        &evp_stats_json,
        vhs_analysis.as_ref(),
        vhs_size,
        vhs_wall_ms,
    )?;

    println!("\n==> Stress test finished successfully!");
    println!("    Report: {}", report_path.display());
    println!(
        "    EVP GIF: {} ({} frames, {} ms, {} bytes)",
        evp_gif.display(),
        evp_analysis.frame_count,
        evp_analysis.total_duration_ms,
        evp_size
    );
    if let Some(ref vhs_path) = vhs_gif {
        println!(
            "    VHS GIF: {} ({} frames, {} bytes)",
            vhs_path.display(),
            vhs_analysis.as_ref().map(|a| a.frame_count).unwrap_or(0),
            vhs_size.unwrap_or(0)
        );
    }

    Ok(())
}

fn run_vhs_comparison(
    root: &Path,
    out_dir: &Path,
    tape_path: &Path,
    vhs_image: &str,
    cpu_pin: &str,
) -> Result<(PathBuf, GifAnalysis, u64, u64)> {
    let vhs_gif = out_dir.join("vhs.gif");
    let mut cmd = Command::new("docker");
    cmd.arg("run").arg("--rm").arg("--cpus=1");

    if !cpu_pin.is_empty() {
        cmd.arg(format!("--cpuset-cpus={cpu_pin}"));
    }

    let rel_tape = tape_path.strip_prefix(root).unwrap_or(tape_path);
    cmd.arg("-v")
        .arg(format!("{}:/work:ro", root.display()))
        .arg("-v")
        .arg(format!("{}:/out", out_dir.display()))
        .arg("-w")
        .arg("/work")
        .arg(vhs_image)
        .arg(format!("/work/{}", rel_tape.display()))
        .arg("-o")
        .arg("/out/vhs.gif");

    let start = Instant::now();
    run_command(&mut cmd, "running VHS in Docker")?;
    let wall_ms = start.elapsed().as_millis() as u64;

    let vhs_size = fs::metadata(&vhs_gif).map(|m| m.len()).unwrap_or(0);
    let analysis = analyze_gif(&vhs_gif, 50, "vhs")?;

    Ok((vhs_gif, analysis, wall_ms, vhs_size))
}

fn generate_report(
    report_path: &Path,
    evp: &GifAnalysis,
    evp_size: u64,
    evp_stats: &Value,
    vhs: Option<&GifAnalysis>,
    vhs_size: Option<u64>,
    vhs_wall_ms: Option<u64>,
) -> Result<()> {
    let evp_wall_ms = evp_stats
        .get("wall_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(evp.total_duration_ms);
    let evp_dropped = evp_stats
        .get("raw_frame_consumer_dropped_frames")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let evp_queue = evp_stats
        .get("max_raw_frame_consumer_queue_len")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut report = String::new();
    report.push_str("# ⚡ evp Stress Test Benchmark Report\n\n");
    report.push_str("## Summary\n\n");
    report.push_str("| Metric | evp | VHS |\n");
    report.push_str("| --- | --- | --- |\n");

    let vhs_wall_str = vhs_wall_ms
        .map(|w| format!("{w} ms"))
        .unwrap_or_else(|| "N/A".to_string());
    let vhs_size_str = vhs_size
        .map(|s| format_bytes(s))
        .unwrap_or_else(|| "N/A".to_string());
    let vhs_frames_str = vhs
        .map(|v| v.frame_count.to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let vhs_skipped_str = vhs
        .map(|v| v.skipped_frames_est.to_string())
        .unwrap_or_else(|| "N/A".to_string());

    report.push_str(&format!(
        "| **Wall Clock** | **{} ms** | {} |\n",
        evp_wall_ms, vhs_wall_str
    ));
    report.push_str(&format!(
        "| **GIF Size** | **{}** ({evp_size} B) | {} |\n",
        format_bytes(evp_size),
        vhs_size_str
    ));
    report.push_str(&format!(
        "| **GIF Frames** | {} | {} |\n",
        evp.frame_count, vhs_frames_str
    ));
    report.push_str(&format!(
        "| **Coalesced/Skipped Frames** | {} | {} |\n",
        evp.skipped_frames_est, vhs_skipped_str
    ));

    if let Some(vhs_w) = vhs_wall_ms {
        if evp_wall_ms > 0 {
            let speedup = vhs_w as f64 / evp_wall_ms as f64;
            report.push_str(&format!(
                "\nVHS wall-clock / evp wall-clock = **{:.2}x** speedup.\n",
                speedup
            ));
        }
    }

    report.push_str("\n## Pipeline Health\n\n");
    report.push_str(&format!(
        "- Dropped raw-frame consumer frames: **{}**\n",
        evp_dropped
    ));
    report.push_str(&format!(
        "- Max runner→raw-frame-consumer queue: **{} / 4096**\n",
        evp_queue
    ));

    report.push_str("\n## EVP Frame Analysis\n\n```json\n");
    report.push_str(&serde_json::to_string_pretty(evp)?);
    report.push_str("\n```\n");

    if let Some(v) = vhs {
        report.push_str("\n## VHS Frame Analysis\n\n```json\n");
        report.push_str(&serde_json::to_string_pretty(v)?);
        report.push_str("\n```\n");
    }

    fs::write(report_path, &report)?;

    if let Ok(step_summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(step_summary)
            .and_then(|mut f| std::io::Write::write_all(&mut f, report.as_bytes()));
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
