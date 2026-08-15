use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::util::{
    cmd::{cargo_build_release_bins, run_command},
    gif::{GifAnalysis, analyze_gif},
    paths::{evp_bin_path, project_root},
};

#[derive(Args, Debug)]
pub struct CompareArgs {
    /// Path to .tape file to compare
    pub tape: PathBuf,

    /// Output directory for generated artifacts
    #[arg(long, short, default_value = "compare-out")]
    pub out_dir: PathBuf,

    /// Docker image for VHS comparison
    #[arg(long, default_value = "ghcr.io/charmbracelet/vhs:v0.11.0")]
    pub vhs_image: String,

    /// Also render SVG output from evp
    #[arg(long)]
    pub svg: bool,

    /// Optional CPU core to pin execution to (e.g. 0)
    #[arg(long)]
    pub cpu_pin: Option<String>,

    /// Skip building release binaries before running
    #[arg(long)]
    pub no_build: bool,
}

pub fn run(args: CompareArgs) -> Result<()> {
    let root = project_root();
    let out_dir = if args.out_dir.is_absolute() {
        args.out_dir
    } else {
        root.join(args.out_dir)
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
    let evp_svg = out_dir.join("evp.svg");
    let vhs_gif = out_dir.join("vhs.gif");

    // 1. Run EVP
    println!("==> Running evp on {}", tape_path.display());
    let mut evp_cmd = if let Some(ref pin) = args.cpu_pin {
        if cfg!(target_os = "linux") {
            let mut c = Command::new("taskset");
            c.arg("-c").arg(pin).arg(&evp_bin);
            c
        } else {
            Command::new(&evp_bin)
        }
    } else {
        Command::new(&evp_bin)
    };

    evp_cmd
        .current_dir(&root)
        .arg(&tape_path)
        .arg("--output")
        .arg(&evp_gif);

    if args.svg {
        evp_cmd.arg("--output").arg(&evp_svg);
    }

    let evp_start = Instant::now();
    run_command(&mut evp_cmd, "running evp")?;
    let evp_wall_ms = evp_start.elapsed().as_millis() as u64;

    let evp_analysis = analyze_gif(&evp_gif, 50, "evp")?;
    let evp_size = fs::metadata(&evp_gif).map(|m| m.len()).unwrap_or(0);

    // 2. Run VHS via Docker
    println!("==> Running VHS on {} in Docker...", tape_path.display());
    let mut vhs_cmd = Command::new("docker");
    vhs_cmd.arg("run").arg("--rm").arg("--cpus=1");

    if let Some(ref pin) = args.cpu_pin {
        vhs_cmd.arg(format!("--cpuset-cpus={pin}"));
    }

    let rel_tape = tape_path.strip_prefix(&root).unwrap_or(&tape_path);
    vhs_cmd
        .arg("-v")
        .arg(format!("{}:/work:ro", root.display()))
        .arg("-v")
        .arg(format!("{}:/out", out_dir.display()))
        .arg("-w")
        .arg("/work")
        .arg(&args.vhs_image)
        .arg(format!("/work/{}", rel_tape.display()))
        .arg("-o")
        .arg("/out/vhs.gif");

    let vhs_start = Instant::now();
    run_command(&mut vhs_cmd, "running VHS in Docker")?;
    let vhs_wall_ms = vhs_start.elapsed().as_millis() as u64;

    let vhs_analysis = analyze_gif(&vhs_gif, 50, "vhs")?;
    let vhs_size = fs::metadata(&vhs_gif).map(|m| m.len()).unwrap_or(0);

    // 3. Print side-by-side comparison
    print_comparison_report(
        &tape_path,
        &evp_analysis,
        evp_size,
        evp_wall_ms,
        &vhs_analysis,
        vhs_size,
        vhs_wall_ms,
    );

    Ok(())
}

fn print_comparison_report(
    tape: &Path,
    evp: &GifAnalysis,
    evp_size: u64,
    evp_wall_ms: u64,
    vhs: &GifAnalysis,
    vhs_size: u64,
    vhs_wall_ms: u64,
) {
    let speedup = if evp_wall_ms > 0 {
        vhs_wall_ms as f64 / evp_wall_ms as f64
    } else {
        1.0
    };

    println!("\n=======================================================");
    println!(
        "  Comparison: {}",
        tape.file_name().and_then(|n| n.to_str()).unwrap_or("")
    );
    println!("=======================================================");
    println!(
        "  {:<24} evp: {:<20} vhs: {}",
        "Wall Clock",
        format!("{evp_wall_ms} ms"),
        format!("{vhs_wall_ms} ms")
    );
    println!(
        "  {:<24} evp: {:<20} vhs: {}",
        "GIF Size",
        format_bytes(evp_size),
        format_bytes(vhs_size)
    );
    println!(
        "  {:<24} evp: {:<20} vhs: {}",
        "GIF Frames", evp.frame_count, vhs.frame_count
    );
    println!(
        "  {:<24} evp: {:<20} vhs: {}",
        "Total Duration",
        format!("{} ms", evp.total_duration_ms),
        format!("{} ms", vhs.total_duration_ms)
    );
    println!(
        "  {:<24} evp: {:<20} vhs: {}",
        "Skipped/Coalesced", evp.skipped_frames_est, vhs.skipped_frames_est
    );
    println!("-------------------------------------------------------");
    println!("  Speedup: **{:.2}x faster** with evp", speedup);
    println!("=======================================================\n");
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.2} MB ({bytes} B)", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB ({bytes} B)", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
