use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::util::{
    cmd::{cargo_build_release_bins, run_command},
    paths::{evp_bin_path, project_root},
};

#[derive(Args, Debug)]
pub struct ExamplesArgs {
    #[command(subcommand)]
    pub command: ExamplesSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum ExamplesSubcommand {
    /// Render one or all examples from examples/*.tape
    Render(RenderArgs),
    /// Parse .stats files from a directory and generate markdown performance summary
    Summary(SummaryArgs),
}

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Name of specific example to render (e.g. hello), or all if omitted
    #[arg(long, short)]
    pub name: Option<String>,

    /// Output directory for rendered examples
    #[arg(long, default_value = "ci/examples")]
    pub out_dir: PathBuf,

    /// Render GIF output
    #[arg(long, default_value_t = true)]
    pub gif: bool,

    /// Render SVG output
    #[arg(long, default_value_t = true)]
    pub svg: bool,

    /// Emit performance stats JSON (.stats)
    #[arg(long, default_value_t = true)]
    pub stats: bool,

    /// Skip building release binaries before rendering
    #[arg(long)]
    pub no_build: bool,
}

#[derive(Args, Debug)]
pub struct SummaryArgs {
    /// Directory containing .stats JSON files
    #[arg(long, short, default_value = "ci/examples")]
    pub dir: PathBuf,
}

pub fn run(args: ExamplesArgs) -> Result<()> {
    match args.command {
        ExamplesSubcommand::Render(r) => run_render(r),
        ExamplesSubcommand::Summary(s) => run_summary(s),
    }
}

fn run_render(args: RenderArgs) -> Result<()> {
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

    let examples_dir = root.join("examples");
    let mut tape_files = Vec::new();
    if let Some(ref specific) = args.name {
        let name = if specific.ends_with(".tape") {
            specific.clone()
        } else {
            format!("{specific}.tape")
        };
        let p = examples_dir.join(&name);
        if !p.exists() {
            bail!("example tape not found: {}", p.display());
        }
        tape_files.push(p);
    } else {
        for entry in fs::read_dir(&examples_dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("tape") {
                tape_files.push(p);
            }
        }
        tape_files.sort();
    }

    println!(
        "==> Rendering {} example(s) to {}",
        tape_files.len(),
        out_dir.display()
    );

    for tape in tape_files {
        let stem = tape.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
        println!("\n--> Rendering example: {stem}");

        let mut cmd = Command::new(&evp_bin);
        cmd.current_dir(&root).arg(&tape);

        if args.gif {
            cmd.arg("--output").arg(out_dir.join(format!("{stem}.gif")));
        }
        if args.svg {
            cmd.arg("--output").arg(out_dir.join(format!("{stem}.svg")));
        }
        if args.stats {
            cmd.arg("--output")
                .arg(out_dir.join(format!("{stem}.stats")));
        }

        run_command(&mut cmd, &format!("rendering {stem}"))?;
    }

    println!(
        "\n✅ All requested examples rendered successfully in {}",
        out_dir.display()
    );
    Ok(())
}

fn run_summary(args: SummaryArgs) -> Result<()> {
    let root = project_root();
    let dir = if args.dir.is_absolute() {
        args.dir
    } else {
        root.join(args.dir)
    };

    if !dir.exists() {
        bail!("stats directory does not exist: {}", dir.display());
    }

    let mut stats_files = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("stats") {
            stats_files.push(p);
        }
    }
    stats_files.sort();

    if stats_files.is_empty() {
        println!("No .stats files found in {}", dir.display());
        return Ok(());
    }

    let mut rows = Vec::new();
    rows.push("### ⚡ Performance Stats Summary\n".to_string());
    rows.push("| Example | Total Duration | Font Init | PTY Spawn | Execution | Captured Frames | Dropped Frames |".to_string());
    rows.push("| --- | --- | --- | --- | --- | --- | --- |".to_string());

    for f in &stats_files {
        let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        if let Ok(content) = fs::read_to_string(f) {
            if let Ok(data) = serde_json::from_str::<Value>(&content) {
                let tel = data.get("telemetry");
                let wall = data.get("wall_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let font = tel
                    .and_then(|t| t.get("font_init"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let pty = tel
                    .and_then(|t| t.get("pty_spawn"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let exec_t = tel
                    .and_then(|t| t.get("runner_execution"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let captured = data
                    .get("captured_frames")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let dropped = data
                    .get("raw_frame_consumer_dropped_frames")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                rows.push(format!(
                    "| **{name}** | {wall}ms | {font}ms | {pty}ms | {exec_t}ms | {captured} | {dropped} |"
                ));
                continue;
            }
        }
        rows.push(format!("| **{name}** | Error reading stats | | | | | |"));
    }

    let summary_md = rows.join("\n") + "\n";
    println!("{summary_md}");

    if let Ok(step_summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(step_summary)
            .and_then(|mut f| std::io::Write::write_all(&mut f, summary_md.as_bytes()));
    }

    Ok(())
}
