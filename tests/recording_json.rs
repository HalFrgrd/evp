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
        assert_eq!(
            a.cursor_color, b.cursor_color,
            "frame {i} cursor_color mismatch"
        );
        assert_eq!(
            a.cursor_accent, b.cursor_accent,
            "frame {i} cursor_accent mismatch"
        );
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

/// When a `Wait` takes longer than the pre-computed timeline end, the
/// recording must extend past that timeline end so frames captured during and
/// after the Wait period are included in the output.
///
/// The shell command uses variable expansion (`${M}DONE`) so the echoed
/// command text does not contain the expected output string `EVPDONE`; Wait
/// can only match once `echo` runs after the sleep.
///
/// Timeline: Sleep 50ms + Type (31 chars × 10ms) + Enter ≈ 360ms; the 200ms
/// tail Sleep sets timeline_end to ~560ms / total_duration to ~693ms.  The
/// command's `sleep 0.5` means output arrives at ~860ms, well past
/// total_duration. Without the fix the recording ends before the Wait matches
/// and the output is never captured.
#[test]
fn wait_extends_recording_when_command_outlasts_timeline() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Set WaitTimeout 5s
Sleep 50ms
Type "M=EVP; sleep 0.5; echo ${M}DONE"
Enter
Wait /EVPDONE/
Sleep 200ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);

    // The output must appear in the recording. Using ${M}DONE means the
    // echoed command line shows "echo ${M}DONE" (not "EVPDONE"), so Wait
    // cannot resolve via the echo — it resolves only when echo prints output.
    assert!(
        haystack.contains("EVPDONE"),
        "expected EVPDONE output to appear in recording after Wait resolved"
    );

    // The recording must continue after the Wait (>= 700ms: ~500ms sleep +
    // ~200ms tail).
    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    assert!(
        last_t >= 700,
        "recording ended too early after Wait; last frame at {last_t}ms (expected >= 700ms)"
    );
}

/// A `Wait` with the `+Screen` scope must also extend the recording window
/// when the matching output arrives after the pre-computed timeline.
#[test]
fn wait_screen_extends_recording_when_command_outlasts_timeline() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Set WaitTimeout 5s
Sleep 50ms
Type "M=SCR; sleep 0.4; echo ${M}DONE"
Enter
Wait Screen /SCRDONE/
Sleep 200ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);

    assert!(
        haystack.contains("SCRDONE"),
        "expected SCRDONE output to appear in recording after Wait+Screen resolved"
    );

    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    assert!(
        last_t >= 500,
        "recording ended too early after Wait+Screen; last frame at {last_t}ms (expected >= 500ms)"
    );
}

/// A timed-out Wait whose timeout is larger than the pre-computed timeline
/// must still extend the recording so it captures at least the timeout
/// duration of frames.
#[test]
fn wait_timeout_extends_recording_past_timeline() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Sleep 50ms
Wait @600ms /__never_matches_wait_test_long__/
Sleep 200ms
"#;

    let rec = record(tape);
    let haystack = full_haystack(&rec);

    // Script still advances after timeout.
    let last_t = rec.frames.last().expect("at least one frame").t_ms();
    // The 600ms timeout fires well past the 250ms timeline_end, so the
    // recording must extend to at least 600ms.
    assert!(
        last_t >= 600,
        "recording ended before timeout duration; last frame at {last_t}ms (expected >= 600ms)"
    );
    let _ = haystack; // used above
}

/// Ensure that no frames captured before the Hide period is over (which ends
/// with Show) contain any of the content typed/produced during the hidden period.
#[test]
fn hide_show_no_leak_before_show() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Sleep 200ms
Hide
Sleep 1s
Type "echo should-be-hidden"
Enter
Sleep 500ms
Show
Sleep 200ms
"#;

    let rec = record(tape);
    // The pre-hide sleep is 200ms, so Hide executes at 200ms.
    // Any frame at t < 200ms must not contain the text "should-be-hidden".
    for i in 0..rec.frames.len() {
        let t = rec.frames[i].t_ms();
        if t < 200 {
            let lines = common::rows_as_strings(&rec, i);
            let frame_text = lines.join("\n");
            assert!(
                !frame_text.contains("should-be-hidden"),
                "frame at {}ms leaked hidden text: {:?}",
                t,
                frame_text
            );
        }
    }
}

#[test]
fn svg_svgz_and_json_screenshots_render_successfully() {
    let temp_dir = std::env::temp_dir();
    let unique_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let svg_path = temp_dir.join(format!("test-shot-{unique_id}.svg"));
    let svgz_path = temp_dir.join(format!("test-shot-{unique_id}.svgz"));
    let json_path = temp_dir.join(format!("test-shot-{unique_id}.json"));

    let tape = format!(
        r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set Shell /bin/sh
Sleep 200ms
Screenshot {svg}
Screenshot {svgz}
Screenshot {json}
"#,
        svg = svg_path.to_str().unwrap(),
        svgz = svgz_path.to_str().unwrap(),
        json = json_path.to_str().unwrap(),
    );

    let _rec = record(&tape);

    assert!(svg_path.exists(), "SVG screenshot file does not exist");
    assert!(svgz_path.exists(), "SVGZ screenshot file does not exist");
    assert!(json_path.exists(), "JSON screenshot file does not exist");

    // Verify SVG content
    let svg_content = std::fs::read_to_string(&svg_path).unwrap();
    assert!(
        svg_content.contains("<svg"),
        "SVG file doesn't contain <svg tag"
    );
    assert!(
        svg_content.contains("</svg>"),
        "SVG file doesn't contain </svg> tag"
    );
    assert!(
        !svg_content.contains("<animate"),
        "SVG screenshot contains <animate element"
    );
    assert!(
        !svg_content.contains("<set"),
        "SVG screenshot contains <set element"
    );

    // Verify SVGZ content (gzip magic bytes)
    let svgz_bytes = std::fs::read(&svgz_path).unwrap();
    assert!(svgz_bytes.len() >= 2);
    assert_eq!(svgz_bytes[0], 0x1f);
    assert_eq!(svgz_bytes[1], 0x8b);

    // Verify JSON content
    let json_content = std::fs::read_to_string(&json_path).unwrap();
    let parsed: evp::Recording = serde_json::from_str(&json_content).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    assert_eq!(parsed.frames[0].t_ms(), 0);

    // Clean up
    let _ = std::fs::remove_file(svg_path);
    let _ = std::fs::remove_file(svgz_path);
    let _ = std::fs::remove_file(json_path);
}

#[test]
fn wait_records_frames_continually() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Sleep 50ms
Hide
Type "sleep 0.1; printf X; sleep 0.15; printf Y; sleep 0.15; printf Z; echo"
Enter
Show
Wait /XYZ/
Sleep 200ms
"#;

    let rec = record(tape);

    let mut saw_x_only = false;
    let mut saw_xy_only = false;
    let mut saw_xyz = false;

    for i in 0..rec.frames.len() {
        let lines = common::rows_as_strings(&rec, i);
        let frame_text = lines.join("\n");
        if frame_text.contains("XYZ") {
            saw_xyz = true;
        } else if frame_text.contains("XY") {
            saw_xy_only = true;
        } else if frame_text.contains("X") {
            saw_x_only = true;
        }
    }

    assert!(saw_x_only, "should have captured intermediate frame with only 'X' during the wait");
    assert!(saw_xy_only, "should have captured intermediate frame with 'XY' during the wait");
    assert!(saw_xyz, "should have captured final frame with 'XYZ' after wait resolved");
}

#[test]
fn hide_show_consecutive_no_frames_between() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Sleep 200ms
Hide
Type "echo first-hidden"
Enter
Sleep 500ms
Show
Hide
Type "echo second-hidden"
Enter
Sleep 500ms
Show
Sleep 200ms
"#;

    let rec = record(tape);

    // Let's inspect all captured frames.
    // If a frame was captured between the first `Show` and the second `Hide`, it would contain
    // "first-hidden" but NOT "second-hidden" (as "second-hidden" was typed during the second hidden period).
    // If no frames were captured between them, then all frames in the recording must either:
    // 1. Contain neither (captured during the initial 200ms sleep before any hidden commands).
    // 2. Contain both (captured during the final 200ms sleep after both hidden commands have run).
    let mut saw_first_hidden = false;
    let mut saw_second_hidden = false;

    for i in 0..rec.frames.len() {
        let lines = common::rows_as_strings(&rec, i);
        let frame_text = lines.join("\n");
        let has_first = frame_text.contains("first-hidden");
        let has_second = frame_text.contains("second-hidden");

        if has_first {
            saw_first_hidden = true;
        }
        if has_second {
            saw_second_hidden = true;
        }

        assert!(
            !(has_first && !has_second),
            "Frame {} (at {}ms) contains 'first-hidden' but not 'second-hidden'. This indicates a frame was leaked/captured between consecutive Show and Hide commands!",
            i,
            rec.frames[i].t_ms()
        );
    }

    assert!(saw_first_hidden, "should have captured frames containing 'first-hidden'");
    assert!(saw_second_hidden, "should have captured frames containing 'second-hidden'");
}

#[test]
fn hide_at_start_no_captured_frames() {
    let tape = r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 10ms
Set Framerate 30
Set Shell /bin/sh
Hide
Sleep 500ms
Show
Sleep 200ms
"#;

    let rec = record(tape);
    // Since Hide is at the very start (t = 0), and then we Sleep 500ms, and then Show, and then Sleep 200ms:
    // - No frames should be captured/recorded during the 500ms sleep.
    // - The only frames captured should be after the Show (which corresponds to the final 200ms).
    // Let's verify the total recorded length/time.
    // The skipped time is 500ms.
    // The total execution time is 700ms.
    // The recorded duration should be around 200ms.
    assert!(!rec.frames.is_empty());
    let max_t = rec.frames.last().unwrap().t_ms();
    assert!(
        max_t < 360,
        "Expected recorded duration to be around 333ms, but got {}ms (frames: {:?})",
        max_t,
        rec.frames.iter().map(|f| f.t_ms()).collect::<Vec<_>>()
    );
}




