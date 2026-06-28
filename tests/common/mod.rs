#![allow(dead_code)]

use evp::Recording;

/// Parse + run a tape source and return the recorded frames.
pub fn record(tape: &str) -> Recording {
    let _ = tracing_subscriber::fmt::try_init();
    let script = evp::parse_script(tape).expect("parse tape");
    let out = evp::run_and_return_recording(&script).expect("run script");
    out.recording
}

/// Round-trip a [`Recording`] through JSON, asserting deterministic bytes.
pub fn json_round_trip(rec: &Recording) -> Recording {
    let bytes = evp::recording_to_json(rec).expect("serialise");
    let parsed = evp::recording_from_json(&bytes).expect("deserialise");
    let bytes2 = evp::recording_to_json(&parsed).expect("serialise round-tripped");
    assert_eq!(
        bytes, bytes2,
        "recording JSON serialisation is not deterministic"
    );
    parsed
}

/// Reconstruct row strings (right-trimmed) for frame index `frame_idx`.
pub fn rows_as_strings(rec: &Recording, frame_idx: usize) -> Vec<String> {
    let frame = rec.reconstruct(frame_idx).expect("reconstruct frame");
    let cols = frame.cols as usize;
    (0..frame.rows as usize)
        .map(|r| {
            let mut line = String::new();
            for c in &frame.cells[r * cols..(r + 1) * cols] {
                if c.text.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&c.text);
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// Concatenate every frame's rendered text into one string.
pub fn full_haystack(rec: &Recording) -> String {
    let mut s = String::new();
    for i in 0..rec.frames.len() {
        for line in rows_as_strings(rec, i) {
            s.push_str(&line);
            s.push('\n');
        }
    }
    s
}

pub fn temp_json_path(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_nanos()
    ))
}

/// Locate the compiled evp_helper_tool binary in the target directories.
pub fn get_helper_bin_path() -> String {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("target/x86_64-unknown-linux-musl/debug/evp_helper_tool"),
        manifest_dir.join("target/x86_64-unknown-linux-musl/release/evp_helper_tool"),
        manifest_dir.join("target/debug/evp_helper_tool"),
        manifest_dir.join("target/release/evp_helper_tool"),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    // Fallback
    "evp_helper_tool".to_string()
}
