use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameInfo {
    pub index: usize,
    pub delay_ms: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GifAnalysis {
    pub label: String,
    pub frame_count: usize,
    pub total_duration_ms: u64,
    pub avg_frame_delay_ms: f64,
    pub skipped_frames_est: usize,
    pub expected_frames: usize,
    pub long_frames: usize,
}

pub fn analyze_gif(path: &Path, expected_fps: u32, label: &str) -> Result<GifAnalysis> {
    let data = fs::read(path).with_context(|| format!("reading GIF: {}", path.display()))?;
    let frames = parse_gif_frames(&data)?;

    let frame_count = frames.len();
    let total_duration_ms: u64 = frames.iter().map(|f| f.delay_ms as u64).sum();
    let avg_frame_delay_ms = if frame_count > 0 {
        total_duration_ms as f64 / frame_count as f64
    } else {
        0.0
    };

    let expected_interval_ms = if expected_fps > 0 {
        1000.0 / expected_fps as f64
    } else {
        20.0
    };

    let expected_frames = if expected_interval_ms > 0.0 {
        (total_duration_ms as f64 / expected_interval_ms).round() as usize
    } else {
        0
    };

    let mut skipped_frames_est = 0;
    let mut long_frames = 0;
    for f in &frames {
        if f.delay_ms as f64 > expected_interval_ms * 1.5 {
            long_frames += 1;
            let missed = (f.delay_ms as f64 / expected_interval_ms).round() as usize;
            if missed > 1 {
                skipped_frames_est += missed - 1;
            }
        }
    }

    Ok(GifAnalysis {
        label: label.to_string(),
        frame_count,
        total_duration_ms,
        avg_frame_delay_ms,
        skipped_frames_est,
        expected_frames,
        long_frames,
    })
}

pub fn parse_gif_frames(data: &[u8]) -> Result<Vec<FrameInfo>> {
    if data.len() < 13 {
        bail!("file too short to be a valid GIF");
    }
    if !data.starts_with(b"GIF87a") && !data.starts_with(b"GIF89a") {
        bail!("invalid GIF header");
    }

    let mut pos = 6;
    let packed = data[pos + 4];
    pos += 7; // Logical Screen Descriptor

    let has_gct = (packed & 0x80) != 0;
    if has_gct {
        let gct_size = 1 << ((packed & 0x07) + 1);
        pos += 3 * gct_size;
    }

    let mut frames = Vec::new();
    let mut current_delay_ms = 0u32;

    while pos < data.len() {
        let block_type = data[pos];
        pos += 1;

        match block_type {
            0x21 => {
                // Extension Block
                if pos >= data.len() {
                    break;
                }
                let ext_label = data[pos];
                pos += 1;

                if ext_label == 0xF9 {
                    // Graphic Control Extension
                    if pos < data.len() {
                        let block_size = data[pos] as usize;
                        pos += 1;
                        if block_size >= 4 && pos + block_size <= data.len() {
                            let delay_lo = data[pos + 1] as u32;
                            let delay_hi = data[pos + 2] as u32;
                            let delay_cs = (delay_hi << 8) | delay_lo;
                            current_delay_ms = delay_cs * 10;
                        }
                        pos += block_size;
                        // Skip sub-blocks if any
                        while pos < data.len() && data[pos] != 0 {
                            let sub_len = data[pos] as usize;
                            pos += 1 + sub_len;
                        }
                        if pos < data.len() && data[pos] == 0 {
                            pos += 1;
                        }
                    }
                } else {
                    // Other Extension: skip sub-blocks
                    while pos < data.len() && data[pos] != 0 {
                        let sub_len = data[pos] as usize;
                        pos += 1 + sub_len;
                    }
                    if pos < data.len() && data[pos] == 0 {
                        pos += 1;
                    }
                }
            }
            0x2C => {
                // Image Descriptor
                if pos + 9 > data.len() {
                    break;
                }
                let img_packed = data[pos + 8];
                pos += 9;

                let has_lct = (img_packed & 0x80) != 0;
                if has_lct {
                    let lct_size = 1 << ((img_packed & 0x07) + 1);
                    pos += 3 * lct_size;
                }

                // LZW minimum code size
                if pos < data.len() {
                    pos += 1;
                }

                // Image data sub-blocks
                while pos < data.len() && data[pos] != 0 {
                    let sub_len = data[pos] as usize;
                    pos += 1 + sub_len;
                }
                if pos < data.len() && data[pos] == 0 {
                    pos += 1;
                }

                frames.push(FrameInfo {
                    index: frames.len(),
                    delay_ms: current_delay_ms,
                });
                current_delay_ms = 0;
            }
            0x3B => {
                // Trailer
                break;
            }
            _ => {
                // Unknown block
                break;
            }
        }
    }

    Ok(frames)
}
