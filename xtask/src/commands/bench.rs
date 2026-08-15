use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use clap::Args;

use crate::util::{cmd::run_command, paths::project_root};

#[derive(Args, Debug)]
pub struct BenchArgs {
    /// Path for benchmark GIF output
    #[arg(long, default_value = "/tmp/evp-benchmark-render.gif")]
    pub out: PathBuf,

    /// Path for benchmark JSON output
    #[arg(long, default_value = "/tmp/evp-benchmark-render.json")]
    pub json_out: PathBuf,
}

pub fn run(args: BenchArgs) -> Result<()> {
    let root = project_root();
    println!("==> Running canonical render benchmark harness...");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&root)
        .arg("run")
        .arg("--release")
        .arg("--example")
        .arg("benchmark_render")
        .arg("--")
        .arg(&args.out)
        .arg(&args.json_out);

    run_command(&mut cmd, "cargo run --release --example benchmark_render")?;
    println!("\n✅ Benchmark completed!");
    println!("   GIF:  {}", args.out.display());
    println!("   JSON: {}", args.json_out.display());
    Ok(())
}
