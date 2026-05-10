//! Background thread that turns the runner's raw frames into a compact
//! diff‑based [`Recording`].

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::recording::{CellChange, Frame, RawFrame, Recording};

// Keep enough buffered frames that short encode spikes do not immediately
// push back on the terminal-driving thread.
const RAW_FRAME_CHANNEL_CAPACITY: usize = 4096;

/// Pipeline-health counters maintained by the encoder thread. Shared with
/// the spawning code so callers can read them after the encoder has
/// joined (the values are monotonic, so reading them post-join needs no
/// synchronisation beyond the atomics themselves).
#[derive(Debug, Default)]
pub struct EncoderStats {
    /// Highest observed `len()` of the encoder's inbound queue
    /// (runner → encoder). Capacity is [`RAW_FRAME_CHANNEL_CAPACITY`].
    pub max_inbound_queue_len: AtomicUsize,
    /// Highest observed `len()` of the renderer tap queue
    /// (encoder → renderer). Zero when no tap is configured.
    pub max_tap_queue_len: AtomicUsize,
    /// Number of frames the encoder couldn't forward to the renderer tap
    /// because the tap queue was full. The recording itself is always
    /// complete; only the rendered output may be missing these.
    pub tap_dropped_frames: AtomicU64,
    /// Number of frames the encoder received from the runner.
    pub frames_received: AtomicU64,
}

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
    /// Live counters updated by the encoder thread. Safe to read after
    /// [`Self::join`] returns.
    pub stats: Arc<EncoderStats>,
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
pub fn spawn(cfg: EncoderConfig, frame_tap: Option<Sender<RawFrame>>) -> EncoderHandle {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) = bounded(RAW_FRAME_CHANNEL_CAPACITY);
    let stats = Arc::new(EncoderStats::default());
    let stats_clone = Arc::clone(&stats);
    let join = thread::Builder::new()
        .name("evp-encoder".into())
        .spawn(move || run(cfg, rx, frame_tap, stats_clone))
        .expect("failed to spawn encoder thread");
    EncoderHandle { tx, join, stats }
}

fn run(
    cfg: EncoderConfig,
    rx: Receiver<RawFrame>,
    frame_tap: Option<Sender<RawFrame>>,
    stats: Arc<EncoderStats>,
) -> Result<Recording> {
    use crossbeam_channel::TrySendError;
    let mut frames: Vec<Frame> = Vec::new();
    let mut last_dense: Option<RawFrame> = None;
    let mut frames_since_key: u32 = 0;

    while let Ok(frame) = rx.recv() {
        // Sample queue depths as soon as we wake up so we observe the
        // pre-pop high-water mark (after pop, len has already decreased).
        let inbound_len = rx.len() + 1; // +1 for the frame we just popped
        bump_max(&stats.max_inbound_queue_len, inbound_len);
        stats.frames_received.fetch_add(1, Ordering::Relaxed);

        if let Some(tap) = &frame_tap {
            // Try to forward to the renderer. We use try_send so a slow
            // renderer never stalls the encoder; the recording is always
            // built completely even if some frames don't make it into the
            // rendered output.
            bump_max(&stats.max_tap_queue_len, tap.len());
            match tap.try_send(frame.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    stats.tap_dropped_frames.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }

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

    let recording = Recording {
        cols: cfg.cols,
        rows: cfg.rows,
        framerate: cfg.framerate,
        cell_width_px: cfg.cell_width_px,
        cell_height_px: cfg.cell_height_px,
        padding_px: cfg.padding_px,
        frames,
    };
    drop(frame_tap);
    Ok(recording)
}

/// Atomic max-update helper: store `val` if it's greater than the
/// currently stored value. Lock-free, monotonic, suitable for
/// high-water-mark counters.
fn bump_max(slot: &AtomicUsize, val: usize) {
    let mut cur = slot.load(Ordering::Relaxed);
    while val > cur {
        match slot.compare_exchange_weak(cur, val, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => cur = observed,
        }
    }
}
