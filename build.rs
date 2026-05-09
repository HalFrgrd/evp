use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use woofwoof::compress;
use vergen_gitcl::{Build, Cargo, Emitter, Gitcl, Rustc};

fn main() -> Result<(), Box<dyn Error>> {
    compress_embedded_fonts()?;

    // Emit build/cargo/git/rustc metadata as VERGEN_* env vars for runtime
    // logging and rich --version output.
    let build = Build::all_build();
    let cargo = Cargo::all_cargo();
    let rustc = Rustc::all_rustc();

    let mut emitter = Emitter::default();
    emitter
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&rustc)?;

    if in_git_worktree() {
        let gitcl = Gitcl::all_git();
        emitter.add_instructions(&gitcl)?;
    } else {
        emit_git_fallbacks();
    }

    emitter.emit()?;

    Ok(())
}

fn compress_embedded_fonts() -> Result<(), Box<dyn Error>> {
    let out_dir = env::var("OUT_DIR")?;
    let out_dir = Path::new(&out_dir);

    // Faces embedded into the renderer. We compress all of them to WOFF2
    // at build time and include the compressed bytes in the final binary.
    let faces = [
        "JetBrainsMonoNerdFontMono-Regular.ttf",
        "JetBrainsMonoNerdFontMono-Bold.ttf",
        "JetBrainsMonoNerdFontMono-Italic.ttf",
        "JetBrainsMonoNerdFontMono-BoldItalic.ttf",
        "unifont_upper-17.0.04.otf",
        "unifont_csur-17.0.04.otf",
    ];

    for face in faces {
        let src = Path::new("assets/fonts").join(face);
        println!("cargo:rerun-if-changed={}", src.display());
        let font = fs::read(&src)?;
        let woff2 = compress(&font, "", 8, true)
            .ok_or_else(|| format!("failed to compress font as WOFF2: {}", src.display()))?;
        let out = out_dir.join(
            face.replace(".ttf", ".woff2")
                .replace(".otf", ".woff2"),
        );
        fs::write(out, woff2)?;
    }

    Ok(())
}

fn in_git_worktree() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|stdout| stdout.trim() == "true")
}

fn emit_git_fallbacks() {
    println!(
        "cargo:warning=git metadata unavailable; using fallback VERGEN_GIT_* values"
    );

    emit_git_fallback(
        "VERGEN_GIT_SHA",
        &["VERGEN_GIT_SHA", "GITHUB_SHA"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_BRANCH",
        &[
            "VERGEN_GIT_BRANCH",
            "GITHUB_HEAD_REF",
            "GITHUB_REF_NAME",
            "GITHUB_REF",
        ],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_DATE",
        &["VERGEN_GIT_COMMIT_DATE"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_TIMESTAMP",
        &["VERGEN_GIT_COMMIT_TIMESTAMP"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_COUNT",
        &["VERGEN_GIT_COMMIT_COUNT"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_AUTHOR_NAME",
        &["VERGEN_GIT_COMMIT_AUTHOR_NAME", "GITHUB_ACTOR"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_AUTHOR_EMAIL",
        &["VERGEN_GIT_COMMIT_AUTHOR_EMAIL"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_COMMIT_MESSAGE",
        &["VERGEN_GIT_COMMIT_MESSAGE"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_DESCRIBE",
        &["VERGEN_GIT_DESCRIBE"],
        "unknown",
    );
    emit_git_fallback(
        "VERGEN_GIT_DIRTY",
        &["VERGEN_GIT_DIRTY"],
        "unknown",
    );
}

fn emit_git_fallback(key: &str, candidates: &[&str], default: &str) {
    let value = candidates
        .iter()
        .find_map(|name| normalized_env_var(name))
        .unwrap_or_else(|| default.to_string());
    println!("cargo:rustc-env={key}={value}");
}

fn normalized_env_var(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}
