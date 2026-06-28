use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use vergen_gitcl::{Build, Cargo, Emitter, Gitcl, Rustc};

fn main() -> Result<(), Box<dyn Error>> {
    verify_prebuilt_libghostty()?;
    compress_embedded_fonts()?;
    extract_ref_script()?;
    update_readme_version()?;

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
        let sha = env::var("VERGEN_GIT_SHA").unwrap_or_else(|_| "unknown".to_string());
        let branch = env::var("VERGEN_GIT_BRANCH").unwrap_or_else(|_| "unknown".to_string());
        let date = env::var("VERGEN_GIT_COMMIT_DATE").unwrap_or_else(|_| "unknown".to_string());
        let dirty = env::var("VERGEN_GIT_DIRTY").unwrap_or_else(|_| "false".to_string());
        println!("cargo:rustc-env=VERGEN_GIT_SHA={sha}");
        println!("cargo:rustc-env=VERGEN_GIT_BRANCH={branch}");
        println!("cargo:rustc-env=VERGEN_GIT_COMMIT_DATE={date}");
        println!("cargo:rustc-env=VERGEN_GIT_DIRTY={dirty}");
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
        "NotoEmoji-Regular",
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

fn extract_ref_script() -> Result<(), Box<dyn Error>> {
    let readme_path = Path::new("README.md");
    println!("cargo:rerun-if-changed={}", readme_path.display());
    let out_dir = env::var("OUT_DIR")?;
    let out_path = Path::new(&out_dir).join("ref_script.tape");
    if !readme_path.exists() {
        fs::write(out_path, "# (Reference tape not available during build)")?;
        return Ok(());
    }
    let readme = fs::read_to_string(readme_path)?;
    let start_marker = "<!-- START_REF_SCRIPT -->";
    let end_marker = "<!-- END_REF_SCRIPT -->";

    if let Some(start_idx) = readme.find(start_marker) {
        if let Some(end_idx) = readme[start_idx..].find(end_marker) {
            let actual_end_idx = start_idx + end_idx;
            let block = &readme[start_idx + start_marker.len()..actual_end_idx];
            let mut inside = false;
            let mut extracted_lines = Vec::new();
            for line in block.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("```") {
                    inside = !inside;
                    continue;
                }
                if inside {
                    extracted_lines.push(line);
                }
            }
            let extracted = extracted_lines.join("\n");
            fs::write(&out_path, extracted)?;
            return Ok(());
        }
    }

    Err("Could not find START_REF_SCRIPT or END_REF_SCRIPT markers in README.md".into())
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

fn update_readme_version() -> Result<(), Box<dyn Error>> {
    let readme_path = Path::new("README.md");
    if !readme_path.exists() {
        return Ok(());
    }
    let readme = fs::read_to_string(readme_path)?;
    let version = env::var("CARGO_PKG_VERSION")?;

    let mut updated = false;
    let mut lines = Vec::new();
    for line in readme.lines() {
        if line.contains("uses: HalFrgrd/evp@v") && line.contains(" # Replace with the desired release tag") {
            let prefix = "uses: HalFrgrd/evp@v";
            let suffix = " # Replace with the desired release tag";
            if let Some(start) = line.find(prefix) {
                if let Some(end) = line.find(suffix) {
                    let old_version_part = &line[start + prefix.len()..end];
                    if old_version_part != version {
                        let mut new_line = String::new();
                        new_line.push_str(&line[..start]);
                        new_line.push_str(prefix);
                        new_line.push_str(&version);
                        new_line.push_str(suffix);
                        lines.push(new_line);
                        updated = true;
                        continue;
                    }
                }
            }
        }
        lines.push(line.to_string());
    }

    if updated {
        let content = lines.join("\n") + "\n";
        fs::write(readme_path, content)?;
    }
    Ok(())
}
