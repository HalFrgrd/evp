//! Background thread that turns the runner's raw frames into a compact
//! diff‑based [`Recording`].

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::{
    FrameStyle,
    recording::{RawFrame, Recording, RecordingBuilder, RecordingConfig},
};

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
    pub frame_style: FrameStyle,
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
pub fn spawn(cfg: EncoderConfig) -> EncoderHandle {
    let (tx, rx): (Sender<RawFrame>, Receiver<RawFrame>) = bounded(RAW_FRAME_CHANNEL_CAPACITY);
    let stats = Arc::new(EncoderStats::default());
    let stats_clone = Arc::clone(&stats);
    let join = thread::Builder::new()
        .name("evp-encoder".into())
        .spawn(move || run(cfg, rx, stats_clone))
        .expect("failed to spawn encoder thread");
    EncoderHandle { tx, join, stats }
}

fn run(cfg: EncoderConfig, rx: Receiver<RawFrame>, stats: Arc<EncoderStats>) -> Result<Recording> {
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
        // Sample queue depths as soon as we wake up so we observe the
        // pre-pop high-water mark (after pop, len has already decreased).
        let inbound_len = rx.len() + 1; // +1 for the frame we just popped
        bump_max(&stats.max_inbound_queue_len, inbound_len);
        stats.frames_received.fetch_add(1, Ordering::Relaxed);

        builder.push_raw(frame);
    }

    Ok(builder.finish())
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
