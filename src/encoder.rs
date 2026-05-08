//! Background thread that turns the runner's raw frames into a compact
//! diff‑based [`Recording`].

use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::recording::{CellChange, Frame, RawFrame, Recording};

/// Configuration constants for the encoder.
#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub padding_px: u32,
    /// Force a full keyframe every N diff frames so that random‑access
    /// reconstruction stays cheap.
    pub keyframe_interval: u32,
}

/// Handle returned by [`spawn`]. The runner pushes frames through `tx`
/// then drops it; calling [`Self::join`] returns the finished recording.
pub struct EncoderHandle {
    pub tx: Sender<RawFrame>,
    pub join: JoinHandle<Result<Recording>>,
}

impl EncoderHandle {
    pub fn join(self) -> Result<Recording> {
        // Dropping `tx` would normally happen at the call site; we drop
        // again here just in case the caller forgot.
        drop(self.tx);
        self.join.join().expect("encoder thread panicked")
    }
}

/// Spawn the encoder thread.
pub fn spawn(cfg: EncoderConfig) -> EncoderHandle {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) = bounded(64);
    let join = thread::Builder::new()
        .name("evp-encoder".into())
        .spawn(move || run(cfg, rx))
        .expect("failed to spawn encoder thread");
    EncoderHandle { tx, join }
}

fn run(cfg: EncoderConfig, rx: Receiver<RawFrame>) -> Result<Recording> {
    let mut frames: Vec<Frame> = Vec::new();
    let mut last_dense: Option<RawFrame> = None;
    let mut frames_since_key: u32 = 0;

    while let Ok(frame) = rx.recv() {
        // Decide between keyframe and diff.
        let is_key = match &last_dense {
            None => true,
            Some(prev) => {
                prev.cols != frame.cols
                    || prev.rows != frame.rows
                    || frames_since_key >= cfg.keyframe_interval
            }
        };

        if is_key {
            frames.push(Frame::Key {
                t_ms: frame.t_ms,
                cursor: frame.cursor,
                default_fg: frame.default_fg,
                default_bg: frame.default_bg,
                cells: frame.cells.clone(),
            });
            frames_since_key = 0;
        } else {
            let prev = last_dense.as_ref().unwrap();
            let mut changes: Vec<CellChange> = Vec::new();
            // The grid sizes match (verified above) so a parallel walk is
            // safe.
            for (idx, (a, b)) in prev.cells.iter().zip(frame.cells.iter()).enumerate() {
                if a != b {
                    changes.push(CellChange {
                        idx: idx as u32,
                        cell: b.clone(),
                    });
                }
            }
            // Even if the grid hasn't changed, the cursor or default colors
            // might have, so we still emit an (empty) diff frame – it's
            // cheap and keeps the timeline aligned with the framerate.
            frames.push(Frame::Diff {
                t_ms: frame.t_ms,
                cursor: frame.cursor,
                default_fg: frame.default_fg,
                default_bg: frame.default_bg,
                changes,
            });
            frames_since_key += 1;
        }

        last_dense = Some(frame);
    }

    Ok(Recording {
        cols: cfg.cols,
        rows: cfg.rows,
        framerate: cfg.framerate,
        cell_width_px: cfg.cell_width_px,
        cell_height_px: cfg.cell_height_px,
        padding_px: cfg.padding_px,
        frames,
    })
}
