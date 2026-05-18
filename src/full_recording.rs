//! Raw-frame consumer that builds a full in-memory [`Recording`].

use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::{
    recording::{RawFrame, Recording, RecordingBuilder, RecordingConfig},
    render_common::{RAW_FRAME_CONSUMER_CHANNEL_CAPACITY, ViewportConfig},
};

pub struct FullRecordingConfig {
    pub viewport: ViewportConfig,
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
        viewport: cfg.viewport,
        keyframe_interval: cfg.keyframe_interval,
    });

    while let Ok(frame) = rx.recv() {
        builder.push_raw(frame);
    }

    Ok(builder.finish())
}
