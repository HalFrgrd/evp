use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};

use crate::util::{cmd::run_command, paths::project_root};

const GHOSTTY_REV: &str = "6590196661f769dd8f2b3e85d6c98262c4ec5b3b";

#[derive(Args, Debug)]
pub struct GhosttyArgs {
    #[command(subcommand)]
    pub command: GhosttySubcommand,
}

#[derive(Subcommand, Debug)]
pub enum GhosttySubcommand {
    /// Extract prebuilt libghostty pkg-config artifacts via Docker Buildx Bake
    Extract(ExtractArgs),
    /// Native cross-compilation of libghostty-vt.a via Zig
    Build(BuildArgs),
}

#[derive(Args, Debug)]
pub struct ExtractArgs {
    /// Destination directory for extracted libghostty artifacts
    #[arg(long, default_value = "assets/libghostty")]
    pub dest: PathBuf,

    /// Docker target platform (e.g. linux/amd64, linux/arm64)
    #[arg(long)]
    pub platform: Option<String>,
}

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Zig target triple (e.g. x86_64-linux-musl, x86_64-freebsd, x86_64-macos, aarch64-macos)
    #[arg(long, default_value = "x86_64-linux-musl")]
    pub target: String,

    /// Destination directory for built libghostty artifacts
    #[arg(long, default_value = "assets/libghostty")]
    pub dest: PathBuf,

    /// Ghostty git commit revision to checkout
    #[arg(long, default_value = GHOSTTY_REV)]
    pub rev: String,
}

pub fn run(args: GhosttyArgs) -> Result<()> {
    match args.command {
        GhosttySubcommand::Extract(e) => run_extract(e),
        GhosttySubcommand::Build(b) => run_build(b),
    }
}

fn run_extract(args: ExtractArgs) -> Result<()> {
    let root = project_root();
    println!("==> Extracting libghostty artifacts via docker buildx bake...");

    let dest = if args.dest.is_absolute() {
        args.dest
    } else {
        root.join(args.dest)
    };

    let mut cmd = Command::new("docker");
    cmd.current_dir(&root).arg("buildx").arg("bake");

    if let Some(ref plat) = args.platform {
        cmd.arg("--set")
            .arg(format!("extract-libghostty.platform={plat}"));
    }

    cmd.arg("--set")
        .arg(format!(
            "extract-libghostty.output=type=local,dest={}",
            dest.display()
        ))
        .arg("extract-libghostty");

    run_command(&mut cmd, "extracting libghostty via docker bake")?;
    println!(
        "\n✅ Successfully extracted libghostty artifacts to {}",
        dest.display()
    );
    Ok(())
}

fn run_build(args: BuildArgs) -> Result<()> {
    let root = project_root();
    let temp_dir = std::env::temp_dir();
    let ghostty_src = temp_dir.join(format!("ghostty-src-{}", std::process::id()));
    let ghostty_install = temp_dir.join(format!("ghostty-install-{}", std::process::id()));

    let dest = if args.dest.is_absolute() {
        args.dest
    } else {
        root.join(args.dest)
    };

    println!("==> Cloning Ghostty at commit {}...", args.rev);
    if ghostty_src.exists() {
        let _ = fs::remove_dir_all(&ghostty_src);
    }
    let mut clone_cmd = Command::new("git");
    clone_cmd
        .arg("clone")
        .arg("--filter=blob:none")
        .arg("--no-checkout")
        .arg("https://github.com/ghostty-org/ghostty.git")
        .arg(&ghostty_src);
    run_command(&mut clone_cmd, "cloning ghostty repository")?;

    let mut checkout_cmd = Command::new("git");
    checkout_cmd
        .current_dir(&ghostty_src)
        .arg("checkout")
        .arg(&args.rev);
    run_command(&mut checkout_cmd, "checking out ghostty commit")?;

    println!(
        "==> Building libghostty-vt for target {} via Zig...",
        args.target
    );
    let mut zig_cmd = Command::new("zig");
    zig_cmd
        .current_dir(&ghostty_src)
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg("-Doptimize=ReleaseFast")
        .arg("-Demit-xcframework=false")
        .arg("-Dapp-runtime=none")
        .arg(format!("-Dtarget={}", args.target))
        .arg("-Dcpu=baseline")
        .arg("--prefix")
        .arg(&ghostty_install);

    run_command(&mut zig_cmd, "compiling libghostty with zig")?;

    println!("==> Staging artifacts into {}...", dest.display());
    fs::create_dir_all(dest.join("lib"))?;
    fs::create_dir_all(dest.join("include"))?;
    fs::create_dir_all(dest.join("share/pkgconfig"))?;

    let src_lib = ghostty_install.join("lib/libghostty-vt.a");
    if !src_lib.exists() {
        bail!("expected static lib not found at {}", src_lib.display());
    }
    fs::copy(&src_lib, dest.join("lib/libghostty-vt.a"))?;

    let src_include = ghostty_install.join("include/ghostty");
    if src_include.exists() {
        copy_dir_all(&src_include, &dest.join("include/ghostty"))?;
    }

    let src_pc = ghostty_install.join("share/pkgconfig/libghostty-vt-static.pc");
    if src_pc.exists() {
        let pc_content = fs::read_to_string(&src_pc)?;
        // Make prefix relative: ${pcfiledir}/../..
        let fixed_pc = pc_content
            .lines()
            .map(|line| {
                if line.starts_with("prefix=") {
                    "prefix=${pcfiledir}/../..".to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            dest.join("share/pkgconfig/libghostty-vt-static.pc"),
            fixed_pc,
        )?;
    }

    // Cleanup temp dirs
    let _ = fs::remove_dir_all(&ghostty_src);
    let _ = fs::remove_dir_all(&ghostty_install);

    println!(
        "\n✅ Successfully built and staged libghostty for {}!",
        args.target
    );
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
