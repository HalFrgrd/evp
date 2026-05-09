use std::error::Error;
use std::process::Command;

use vergen_gitcl::{Build, Cargo, Emitter, Gitcl, Rustc};

fn main() -> Result<(), Box<dyn Error>> {
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
    println!("cargo:rustc-env=VERGEN_GIT_SHA=unknown");
    println!("cargo:rustc-env=VERGEN_GIT_BRANCH=unknown");
    println!("cargo:rustc-env=VERGEN_GIT_COMMIT_DATE=unknown");
    println!("cargo:rustc-env=VERGEN_GIT_DIRTY=unknown");
}
