//! Raw-frame consumer that builds a full in-memory [`Recording`].

use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::{
    FrameStyle,
    recording::{RawFrame, Recording, RecordingBuilder, RecordingConfig},
    render_common::RAW_FRAME_CONSUMER_CHANNEL_CAPACITY,
};

pub struct FullRecordingConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub frame_style: FrameStyle,
    pub keyframe_interval: u32,
}

pub struct FullRecording {
    pub tx: Sender<RawFrame>,
    join: JoinHandle<Result<Recording>>,
}

impl FullRecording {
    pub fn join(self) -> Result<Recording> {
        drop(self.tx);
        self.join.join().expect("full recording worker panicked")
    }
}

pub fn spawn_full_recording(cfg: FullRecordingConfig) -> FullRecording {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) =
        bounded(RAW_FRAME_CONSUMER_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("evp-full-recording".into())
        .spawn(move || run_full_recording_worker(rx, cfg))
        .expect("failed to spawn full recording worker");
    FullRecording { tx, join }
}

fn run_full_recording_worker(
    rx: Receiver<RawFrame>,
    cfg: FullRecordingConfig,
) -> Result<Recording> {
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

    Ok(builder.finish())
}
