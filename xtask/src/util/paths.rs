use std::path::{Path, PathBuf};

pub fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cargo manifest dir parent")
        .to_path_buf()
}

#[allow(dead_code)]
pub fn target_dir() -> PathBuf {
    project_root().join("target")
}

pub fn evp_bin_path() -> PathBuf {
    let root = project_root();
    let musl_path = root.join("target/x86_64-unknown-linux-musl/release/evp");
    if musl_path.exists() {
        return musl_path;
    }
    let default_path = root.join("target/release/evp");
    if default_path.exists() {
        return default_path;
    }
    musl_path
}

#[allow(dead_code)]
pub fn evp_helper_tool_bin_path() -> PathBuf {
    let root = project_root();
    let musl_path = root.join("target/x86_64-unknown-linux-musl/release/evp_helper_tool");
    if musl_path.exists() {
        return musl_path;
    }
    let default_path = root.join("target/release/evp_helper_tool");
    if default_path.exists() {
        return default_path;
    }
    musl_path
}
