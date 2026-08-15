use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub fn run_command(cmd: &mut Command, desc: &str) -> Result<()> {
    println!("==> {desc}");
    let status = cmd
        .status()
        .with_context(|| format!("failed to run {desc}"))?;
    if !status.success() {
        bail!("{desc} failed with exit code: {:?}", status.code());
    }
    Ok(())
}

#[allow(dead_code)]
pub fn run_command_capture(cmd: &mut Command, desc: &str) -> Result<String> {
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run {desc}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{desc} failed with exit code {:?}:\n{stderr}",
            output.status.code()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn cargo_build_release_bins(root: &Path) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .arg("build")
        .arg("--workspace")
        .arg("--release")
        .arg("--bins");

    run_command(&mut cmd, "cargo build --workspace --release --bins")
}
