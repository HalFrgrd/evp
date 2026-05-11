//! Integration tests for end-to-end recording + JSON serialisation.
//!
//! These tests exercise the public `evp` library API directly — no
//! subprocess, no `LD_LIBRARY_PATH` shenanigans, no on-disk artifacts
//! beyond what individual tests opt into. The pipeline under test is:
//!
//! ```text
//! parse_script -> run -> Recording -> recording_to_json -> recording_from_json
//! ```
//!
//! Each test parses a tape source, runs it against a real `/bin/sh` in a
//! pseudo-terminal, then asserts on either the in-memory `Recording` or
//! its JSON round-trip.

use evp::{Frame, Recording};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse + run a tape source and return the recorded frames. A panic
/// here means the pipeline itself is broken; individual tests assert on
/// the returned recording's contents.
fn record(tape: &str) -> Recording {
    let script = evp::parse_script(tape).expect("parse tape");
    let out = evp::run(&script).expect("run script");
    out.recording
}

/// Round-trip a [`Recording`] through JSON, asserting the result is
/// byte-identical to a second serialisation of the deserialised value.
/// This catches non-determinism (e.g. accidental `HashMap` use) and any
/// silent loss of fields.
fn json_round_trip(rec: &Recording) -> Recording {
    let bytes = evp::recording_to_json(rec).expect("serialise");
    let parsed = evp::recording_from_json(&bytes).expect("deserialise");
    let bytes2 = evp::recording_to_json(&parsed).expect("serialise round-tripped");
    assert_eq!(
        bytes, bytes2,
        "recording JSON serialisation is not deterministic"
    );
    parsed
}

/// Reconstruct row strings (whitespace-trimmed on the right) for the
/// frame at `frame_idx` using the public reconstruction helper.
fn rows_as_strings(rec: &Recording, frame_idx: usize) -> Vec<String> {
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

/// Concatenate every frame's rendered text into one big haystack for
/// substring assertions.
fn full_haystack(rec: &Recording) -> String {
    let mut s = String::new();
    for i in 0..rec.frames.len() {
        for line in rows_as_strings(rec, i) {
            s.push_str(&line);
            s.push('\n');
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A minimal sleep-only script must produce a structurally well-formed
/// recording: positive geometry, monotonic timestamps, the first frame is
/// a keyframe at `t=0`, and any subsequent keyframes are spaced at
/// `framerate * 5` (the configured `keyframe_interval`).
#[test]
fn empty_sleep_script_produces_well_formed_recording() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set Framerate 30
Set Shell /bin/sh
Sleep 500ms
"#;
    let rec = record(tape);

    assert_eq!(rec.framerate, 30);
    assert!(rec.cols >= 20, "cols too small: {}", rec.cols);
    assert!(rec.rows >= 5, "rows too small: {}", rec.rows);
    assert!(rec.cell_width_px > 0);
    assert!(rec.cell_height_px > 0);
    assert!(rec.frame_style.padding_px > 0);

    assert!(!rec.frames.is_empty(), "no frames captured");
    assert!(matches!(rec.frames[0], Frame::Key { .. }));
    assert_eq!(rec.frames[0].t_ms(), 0, "first frame must start at t=0");

    for pair in rec.frames.windows(2) {
        assert!(
            pair[1].t_ms() > pair[0].t_ms(),
            "non-monotonic frame timestamps: {} -> {}",
            pair[0].t_ms(),
            pair[1].t_ms(),
        );
    }

    let last_t = rec.frames.last().unwrap().t_ms();
    assert!(
        last_t <= 2_000,
        "last frame timestamp drifted way past the 500ms script: {last_t}ms"
    );

    let key_indices: Vec<usize> = rec
        .frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| matches!(f, Frame::Key { .. }).then_some(i))
        .collect();
    assert_eq!(key_indices[0], 0, "first keyframe must be at index 0");
    for pair in key_indices.windows(2) {
        let gap = pair[1] - pair[0];
        assert_eq!(
            gap,
            (rec.framerate as usize) * 5,
            "unexpected keyframe spacing"
        );
    }
}

/// End-to-end content test: typed input must reach the shell, be echoed
/// back, and end up in the recorded cells.
#[test]
fn typed_input_appears_in_recorded_cells() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 20ms
Set Framerate 30
Set Shell /bin/sh
Sleep 500ms
Type "echo hello evp"
Enter
Sleep 1s
"#;
    let rec = record(tape);
    let haystack = full_haystack(&rec);
    assert!(
        haystack.contains("echo hello evp"),
        "did not see typed command in any frame; haystack tail:\n{}",
        &haystack[haystack.len().saturating_sub(2_000)..]
    );
    assert!(
        haystack.contains("hello evp"),
        "did not see shell output in any frame"
    );
}

/// Cursor position is captured into every frame. With a vanilla `/bin/sh`
/// and no input, the cursor must be visible and inside the grid.
#[test]
fn cursor_position_is_recorded() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set Framerate 30
Set Shell /bin/sh
Sleep 500ms
"#;
    let rec = record(tape);
    let last = rec.frames.last().expect("at least one frame");
    let (cx, cy) = last.cursor().expect("cursor should be visible");
    assert!(
        cx < rec.cols,
        "cursor x={cx} out of bounds (cols={})",
        rec.cols
    );
    assert!(
        cy < rec.rows,
        "cursor y={cy} out of bounds (rows={})",
        rec.rows
    );
}

/// JSON round-trip must preserve every field used by downstream
/// consumers (renderer, screenshot exporter, future svg renderer).
#[test]
fn recording_round_trips_through_json() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 20ms
Set Framerate 30
Set Shell /bin/sh
Sleep 200ms
Type "hi"
Sleep 200ms
"#;
    let rec = record(tape);
    let parsed = json_round_trip(&rec);

    // Top-level metadata.
    assert_eq!(parsed.cols, rec.cols);
    assert_eq!(parsed.rows, rec.rows);
    assert_eq!(parsed.framerate, rec.framerate);
    assert_eq!(parsed.cell_width_px, rec.cell_width_px);
    assert_eq!(parsed.cell_height_px, rec.cell_height_px);
    assert_eq!(parsed.frame_style, rec.frame_style);
    assert_eq!(parsed.frames.len(), rec.frames.len());

    // Reconstructed cells must match index-by-index for every frame.
    for i in 0..rec.frames.len() {
        let a = rec.reconstruct(i).unwrap();
        let b = parsed.reconstruct(i).unwrap();
        assert_eq!(a.t_ms, b.t_ms, "frame {i} t_ms mismatch");
        assert_eq!(a.cursor, b.cursor, "frame {i} cursor mismatch");
        assert_eq!(a.cells, b.cells, "frame {i} cells mismatch");
    }
}

#[test]
fn render_json_writes_intermediate_recording() {
    let tape = r#"
Output out.json
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 20ms
Set Framerate 30
Set Shell /bin/sh
Sleep 200ms
Type "json"
Sleep 200ms
"#;
    let rec = record(tape);
    let path = std::env::temp_dir().join(format!(
        "evp-render-json-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    evp::render_json(&rec, &path).expect("render json");
    let bytes = std::fs::read(&path).expect("read json");
    let parsed = evp::recording_from_json(&bytes).expect("parse json");
    std::fs::remove_file(&path).ok();
    assert_eq!(parsed.frames.len(), rec.frames.len());
    assert_eq!(parsed.frame_style, rec.frame_style);
}

/// `Hide`/`Show` should pause and resume frame recording without pausing
/// script execution. Hidden wall-clock time must not appear as a large
/// timestamp gap in the JSON intermediate format.
#[test]
fn hide_show_skips_hidden_time_in_json_recording() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 20ms
Set Framerate 30
Set Shell /bin/sh
Sleep 300ms
Type "echo before-visible"
Enter
Sleep 300ms
Hide
Sleep 2s
Type "echo hidden-ran"
Enter
Sleep 300ms
Show
Sleep 300ms
Type "echo after-visible"
Enter
Sleep 300ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);
    assert!(
        haystack.contains("before-visible"),
        "expected visible pre-hide output to be recorded"
    );
    assert!(
        haystack.contains("hidden-ran"),
        "expected hidden commands to execute and their final state to appear after Show"
    );
    assert!(
        haystack.contains("after-visible"),
        "expected visible post-show output to be recorded"
    );

    let bytes = evp::recording_to_json(&rec).expect("serialise recording");
    let json: Value = serde_json::from_slice(&bytes).expect("parse recording json");
    let frames = json["frames"].as_array().expect("frames is array");
    assert!(!frames.is_empty(), "expected at least one frame");

    let t_values: Vec<u32> = frames
        .iter()
        .map(|f| f["t_ms"].as_u64().expect("frame has t_ms") as u32)
        .collect();
    let max_gap = t_values.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    assert!(
        max_gap < 1_000,
        "hidden section leaked into recording timeline; max frame gap was {max_gap}ms"
    );
}
