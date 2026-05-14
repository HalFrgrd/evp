//! Integration tests for end-to-end recording + JSON serialisation.
//!
//! These tests exercise the public `evp` library API directly — no
//! subprocess, no `LD_LIBRARY_PATH` shenanigans, no on-disk artifacts
//! beyond what individual tests opt into. The pipeline under test is:
//!
//! ```text
//! parse_script -> run_and_return_recording -> Recording -> recording_to_json -> recording_from_json
//! ```
//!
//! Each test parses a tape source, runs it against a real `/bin/sh` in a
//! pseudo-terminal, then asserts on either the in-memory `Recording` or
//! its JSON round-trip.

mod common;

use common::{full_haystack, json_round_trip, record, temp_json_path};
use evp::Frame;
use serde_json::Value;

const WAIT_MATCH_MAX_MS: u32 = 2_500;
const WAIT_TIMEOUT_MIN_MS: u32 = 700;

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

/// Cursor position is captured into frames. With a vanilla `/bin/sh`
/// and no input, the cursor must be visible and inside the grid for at
/// least one frame (the exact frame depends on blink phase, which depends
/// on when the cursor last moved).
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
    // Find every frame where the cursor is visible.
    let visible: Vec<(u16, u16)> = rec.frames.iter().filter_map(|f| f.cursor()).collect();
    assert!(
        !visible.is_empty(),
        "cursor was never visible in any frame (blink may have hidden it on every sampled frame)"
    );
    for (cx, cy) in &visible {
        assert!(
            *cx < rec.cols,
            "cursor x={cx} out of bounds (cols={})",
            rec.cols
        );
        assert!(
            *cy < rec.rows,
            "cursor y={cy} out of bounds (rows={})",
            rec.rows
        );
    }
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
    let path = temp_json_path("evp-render-json");

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

#[test]
fn wait_line_regex_unblocks_on_matching_output() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Set WaitTimeout 2s
Sleep 200ms
Type "printf wait-line-ok; sleep 1"
Enter
Wait /wait-line-ok/
Type "echo after-wait-line"
Enter
Sleep 300ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);
    assert!(
        haystack.contains("after-wait-line"),
        "expected commands after Wait to run"
    );

    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    assert!(
        last_t < WAIT_MATCH_MAX_MS,
        "Wait likely timed out instead of matching quickly; last frame at {last_t}ms"
    );
}

#[test]
fn wait_screen_regex_handles_escaped_metacharacter_literal() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Set WaitTimeout 2s
Sleep 200ms
Type "printf sum+one; sleep 1"
Enter
Wait Screen /sum\+one/
Type "echo after-wait-screen"
Enter
Sleep 300ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);
    assert!(
        haystack.contains("after-wait-screen"),
        "expected commands after Screen Wait to run"
    );

    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    assert!(
        last_t < WAIT_MATCH_MAX_MS,
        "Screen Wait likely timed out; last frame at {last_t}ms"
    );
}

#[test]
fn wait_timeout_still_advances_script() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Sleep 200ms
Wait @400ms /__never_matches_wait_test__/
Type "echo after-timeout"
Enter
Sleep 300ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);
    assert!(
        haystack.contains("after-timeout"),
        "expected script to continue after Wait timeout"
    );

    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    assert!(
        last_t >= WAIT_TIMEOUT_MIN_MS,
        "expected at least ~400ms timeout + surrounding sleeps; got {last_t}ms"
    );
}
