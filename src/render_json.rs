//! JSON raw-frame consumer for the intermediate [`Recording`] format.

use std::{
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::recording::{RawFrame, Recording, RecordingBuilder, RecordingConfig};
use crate::render_common::RAW_FRAME_CONSUMER_CHANNEL_CAPACITY;

pub struct JsonStreamConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub frame_style: crate::FrameStyle,
    pub keyframe_interval: u32,
}

pub struct JsonStreamHandle {
    pub tx: Sender<RawFrame>,
    join: JoinHandle<Result<()>>,
}

impl JsonStreamHandle {
    pub fn join(self) -> Result<()> {
        drop(self.tx);
        self.join.join().expect("json stream worker panicked")
    }
}

pub fn spawn_json_stream(cfg: JsonStreamConfig, output: PathBuf) -> Result<JsonStreamHandle> {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RAW_FRAME_CONSUMER_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-json-stream".into())
        .spawn(move || run_json_stream_worker(rx, cfg, output))
        .expect("failed to spawn json stream worker");
    Ok(JsonStreamHandle { tx, join })
}

pub fn render_json(rec: &Recording, out: &Path) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(rec).context("serialising recording")?;
    std::fs::write(out, bytes).with_context(|| format!("writing {}", out.display()))
}

fn run_json_stream_worker(
    rx: Receiver<RawFrame>,
    cfg: JsonStreamConfig,
    output: PathBuf,
) -> Result<()> {
    let mut builder = RecordingBuilder::new(RecordingConfig {
        cols: cfg.cols,
        rows: cfg.rows,
        framerate: cfg.framerate,
        cell_width_px: cfg.cell_width_px,
        cell_height_px: cfg.cell_height_px,
        frame_style: cfg.frame_style,
        keyframe_interval: cfg.keyframe_interval,
    });

    while let Ok(frame) = rx.recv() {
        builder.push_raw(frame);
    }

    let recording = builder.finish();
    let bytes = serde_json::to_vec_pretty(&recording).context("serialising recording")?;
    std::fs::write(&output, bytes).with_context(|| format!("writing {}", output.display()))?;
    if recording.frames.is_empty() {
        return Err(anyhow!("json renderer received no frames"));
    }
    Ok(())
}
