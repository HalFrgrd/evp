use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::util::{
    cmd::{cargo_build_release_bins, run_command},
    paths::{evp_bin_path, project_root},
};

#[derive(Args, Debug)]
pub struct CiArgs {
    /// Skip cargo fmt check
    #[arg(long)]
    pub skip_fmt: bool,

    /// Skip cargo clippy check
    #[arg(long)]
    pub skip_clippy: bool,

    /// Skip cargo test suite
    #[arg(long)]
    pub skip_tests: bool,

    /// Skip subcommand and tape smoke test
    #[arg(long)]
    pub skip_smoke: bool,

    /// Target architecture to test (e.g. x86_64-unknown-linux-musl)
    #[arg(long)]
    pub target: Option<String>,
}

pub fn run(args: CiArgs) -> Result<()> {
    let root = project_root();
    println!("==> Running CI validation suite in {}", root.display());

    // 1. Cargo fmt check
    if !args.skip_fmt {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root).arg("fmt").arg("--check");
        run_command(&mut cmd, "cargo fmt --check")?;
    }

    // 2. Clippy check
    if !args.skip_clippy {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root)
            .arg("clippy")
            .arg("--workspace")
            .arg("--all-targets");
        if let Some(ref t) = args.target {
            cmd.arg("--target").arg(t);
        }
        run_command(&mut cmd, "cargo clippy --workspace --all-targets")?;
    }

    // 3. Cargo test suite
    if !args.skip_tests {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root).arg("test").arg("--workspace");
        if let Some(ref t) = args.target {
            cmd.arg("--target").arg(t);
        }
        run_command(&mut cmd, "cargo test --workspace")?;
    }

    // 4. Subcommand smoke test
    if !args.skip_smoke {
        println!("==> Building release binaries for smoke test...");
        cargo_build_release_bins(&root)?;

        let evp_bin = evp_bin_path();
        if !evp_bin.exists() {
            bail!("evp binary not found at {}", evp_bin.display());
        }

        let temp_dir = std::env::temp_dir();
        let smoke_tape = temp_dir.join("evp-ci-smoke.tape");
        let smoke_gif = temp_dir.join("evp-ci-smoke.gif");

        println!("==> Running smoke test: print-ref-script");
        let mut print_cmd = Command::new(&evp_bin);
        print_cmd.current_dir(&root).arg("print-ref-script");
        let output = print_cmd
            .output()
            .context("failed to run evp print-ref-script")?;
        if !output.status.success() {
            bail!("evp print-ref-script failed");
        }
        fs::write(&smoke_tape, output.stdout)?;

        println!(
            "==> Running smoke test: render ref script to {}",
            smoke_gif.display()
        );
        let mut render_cmd = Command::new(&evp_bin);
        render_cmd
            .current_dir(&root)
            .arg(&smoke_tape)
            .arg("--output")
            .arg(&smoke_gif);
        run_command(&mut render_cmd, "rendering smoke test GIF")?;

        if !smoke_gif.exists() || fs::metadata(&smoke_gif)?.len() == 0 {
            bail!("smoke test GIF was not produced or is empty");
        }
        println!(
            "==> Smoke test passed (GIF size: {} bytes)",
            fs::metadata(&smoke_gif)?.len()
        );
    }

    println!("\n✅ All CI checks passed successfully!");
    Ok(())
}
