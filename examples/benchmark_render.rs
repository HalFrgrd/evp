use std::{
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use evp::{Frame, FrameStyle, Recording, RenderOptions};

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/evp-benchmark-render.gif"));
    let json_out = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/evp-benchmark-render.json"));

    // Keep a small render loop so we benchmark rendering repeatedly from
    // one captured recording, not repeated capture workflows.
    const RENDER_REPEATS: u32 = 4;

    let fixture_dir = create_fixture_fs()?;
    let script = build_script(&fixture_dir);
    let script_out = fixture_dir.join("benchmark.tape");
    fs::write(&script_out, &script).with_context(|| format!("writing {}", script_out.display()))?;

    let rec_build_start = Instant::now();
    let rec = capture_once_from_script(&script)?;
    let rec_build_elapsed = rec_build_start.elapsed();

    let json_bytes = evp::recording_to_json(&rec)?;
    std::fs::write(&json_out, &json_bytes)?;

    let key_frames = rec
        .frames
        .iter()
        .filter(|f| matches!(f, Frame::Key { .. }))
        .count();
    let diff_frames = rec.frames.len() - key_frames;

    let render_opts = crate::RenderOptions {
        font_path: None,
        font_size: 22.0,
        line_height: 1.0,
        letter_spacing: 1.0,
        frame_style: FrameStyle::default(),
        no_system_fonts: false,
    };

    let mut render_ms_total = 0u128;
    let mut render_ms_last = 0u128;
    for i in 0..RENDER_REPEATS {
        let target = if i + 1 == RENDER_REPEATS {
            out.clone()
        } else {
            out.with_extension(format!("pass-{}.gif", i + 1))
        };
        let render_start = Instant::now();
        evp::render_gif(&rec, &render_opts, &target)?;
        let elapsed = render_start.elapsed().as_millis();
        render_ms_total += elapsed;
        render_ms_last = elapsed;
    }

    println!("recording_build_ms={}", rec_build_elapsed.as_millis());
    println!("recording_json_bytes={}", json_bytes.len());
    println!("key_frames={} diff_frames={}", key_frames, diff_frames);
    println!("render_repeats={}", RENDER_REPEATS);
    println!("render_total_ms={}", render_ms_total);
    println!("render_avg_ms={}", render_ms_total / RENDER_REPEATS as u128);
    println!("render_last_ms={}", render_ms_last);
    println!(
        "frames={} cols={} rows={} framerate={} fixture={} script={} output={} json={}",
        rec.frames.len(),
        rec.cols,
        rec.rows,
        rec.framerate,
        fixture_dir.display(),
        script_out.display(),
        out.display(),
        json_out.display()
    );

    Ok(())
}

fn capture_once_from_script(script_src: &str) -> anyhow::Result<Recording> {
    let script = evp::parse_script(script_src)?;
    let out = evp::run_and_return_recording(&script)?;
    Ok(out.recording)
}

fn create_fixture_fs() -> anyhow::Result<PathBuf> {
    let mut p = std::env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before UNIX_EPOCH")?
        .as_millis();
    p.push(format!("evp-bench-fs-{}-{}", std::process::id(), stamp));

    fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
    fs::create_dir_all(p.join("alpha_dir/nested"))?;
    fs::create_dir_all(p.join("long-directory-name-for-tab-completion"))?;
    fs::create_dir_all(p.join("bin"))?;

    fs::write(p.join("alpha_one.txt"), "alpha one\n")?;
    fs::write(p.join("alpha_two.log"), "alpha two\n")?;
    fs::write(p.join("alpha_dir/nested/readme.md"), "nested file\n")?;
    fs::write(
        p.join("long-directory-name-for-tab-completion/sample.txt"),
        "sample\n",
    )?;
    fs::write(p.join("bin/run-me.sh"), "#!/bin/sh\necho run\n")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(p.join("bin/run-me.sh"))?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(p.join("bin/run-me.sh"), perms)?;
    }

    Ok(p)
}

fn build_script(fixture_dir: &Path) -> String {
    // Keep this script single-run and representative: colored listings +
    // readline tab completion in a non-trivial directory tree.
    format!(
        r#"Set Shell /bin/bash
Set Width 1200
Set Height 700
Set FontSize 18
Set TypingSpeed 80ms
Set Framerate 30
Env TERM xterm-256color
Env BENCH_DIR {bench_dir}

Sleep 400ms
Type "export PS1='$ '"
Enter
    Sleep 350ms

Type "cd $BENCH_DIR"
Enter
    Sleep 350ms

Type "ls --color=always"
Enter
    Sleep 350ms

Type "ls --color=always -lah"
Enter
    Sleep 350ms

Type "cat alpha"
Tab
Tab
Type "_one.txt"
Enter
    Sleep 450ms

Type "ls long"
Tab
Enter
    Sleep 350ms

Type "ls alpha_dir/nes"
Tab
Enter
    Sleep 450ms

Sleep 600ms
"#,
        bench_dir = fixture_dir.display()
    )
}
