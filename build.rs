use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use vergen_gitcl::{Build, Cargo, Emitter, Gitcl, Rustc};

fn main() -> Result<(), Box<dyn Error>> {
    verify_prebuilt_libghostty()?;
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
    }

    emitter.emit()?;

    Ok(())
}

fn verify_prebuilt_libghostty() -> Result<(), Box<dyn Error>> {
    if env::var_os("GHOSTTY_SOURCE_DIR").is_some() {
        return Err("GHOSTTY_SOURCE_DIR must be unset for evp builds; evp requires prebuilt pkg-config assets in assets/libghostty. Remove GHOSTTY_SOURCE_DIR and rerun.".into());
    }

    let required = [
        "assets/libghostty/lib/libghostty-vt.a",
        "assets/libghostty/include/ghostty/vt.h",
        "assets/libghostty/share/pkgconfig/libghostty-vt-static.pc",
    ];

    let missing = required
        .iter()
        .filter(|p| !Path::new(p).exists())
        .copied()
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        let joined = missing.join(", ");
        return Err(format!(
            "missing prebuilt libghostty assets: {joined}. Run `docker buildx bake extract-libghostty` from the evp repo root before building."
        )
        .into());
    }

    Ok(())
}

fn compress_embedded_fonts() -> Result<(), Box<dyn Error>> {
    let out_dir = env::var("OUT_DIR")?;
    let out_dir = Path::new(&out_dir);

    // All fonts are pre-compressed to WOFF2 in assets/fonts/*.woff2
    // Just copy them to OUT_DIR for embedding in the binary.
    let faces = [
        "JetBrainsMonoNerdFontMono-Regular",
        "JetBrainsMonoNerdFontMono-Bold",
        "JetBrainsMonoNerdFontMono-Italic",
        "JetBrainsMonoNerdFontMono-BoldItalic",
        "NotoSansMono-Regular",
        "NotoSansSymbols2-Regular",
        "NotoSansMonoCJKjp-Subset",
        "unifont_upper-17.0.04",
        "unifont_csur-17.0.04",
    ];

    for face in faces {
        let src = Path::new("assets/fonts").join(format!("{}.woff2", face));
        println!("cargo:rerun-if-changed={}", src.display());
        let out = out_dir.join(format!("{}.woff2", face));
        fs::copy(&src, &out)?;
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
