use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::Sender;

use crate::{
    FrameStyle, RawFrame, Recording, RenderOptions, SvgOptions,
    render_gif::{self, GifStreamConfig, GifStreamHandle},
    render_json::{self, JsonStreamConfig, JsonStreamHandle},
    render_svg::{self, SvgStreamConfig, SvgStreamHandle},
};

pub enum RendererBackend {
    Gif(RenderOptions),
    Svg(SvgOptions),
    Json,
}

pub struct RendererConfig {
    pub cols: u16,
    pub rows: u16,
    pub framerate: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub frame_style: FrameStyle,
}

enum RendererJoin {
    Gif(GifStreamHandle),
    Svg(SvgStreamHandle),
    Json(JsonStreamHandle),
}

pub struct RendererHandle {
    pub tx: Sender<RawFrame>,
    join: RendererJoin,
}

impl RendererHandle {
    pub fn join(self) -> Result<()> {
        // Destructure so we can explicitly drop `tx` BEFORE joining the
        // backend. `tx` is a clone of the worker's sender; the inner handle
        // holds the original. Both must be dropped for the worker's
        // `rx.recv()` to return Err and the worker to exit. If we instead
        // call `h.join()` (which blocks waiting for the worker) while
        // `self.tx` is still alive, we deadlock.
        let RendererHandle { tx, join } = self;
        drop(tx);
        match join {
            RendererJoin::Gif(h) => h.join(),
            RendererJoin::Svg(h) => h.join(),
            RendererJoin::Json(h) => h.join(),
        }
    }
}

pub fn spawn_renderer(
    cfg: RendererConfig,
    backend: RendererBackend,
    output: PathBuf,
) -> Result<RendererHandle> {
    match backend {
        RendererBackend::Gif(opts) => {
            let h = render_gif::spawn_gif_stream(
                GifStreamConfig {
                    cols: cfg.cols,
                    rows: cfg.rows,
                },
                opts,
                output,
            )
            .context("spawning gif renderer")?;
            Ok(RendererHandle {
                tx: h.tx.clone(),
                join: RendererJoin::Gif(h),
            })
        }
        RendererBackend::Svg(opts) => {
            let h = render_svg::spawn_svg_stream(
                SvgStreamConfig {
                    cols: cfg.cols,
                    rows: cfg.rows,
                    framerate: cfg.framerate,
                    cell_width_px: cfg.cell_width_px,
                    cell_height_px: cfg.cell_height_px,
                    frame_style: cfg.frame_style,
                },
                opts,
                output,
            )
            .context("spawning svg renderer")?;
            Ok(RendererHandle {
                tx: h.tx.clone(),
                join: RendererJoin::Svg(h),
            })
        }
        RendererBackend::Json => {
            let h = render_json::spawn_json_stream(
                JsonStreamConfig {
                    cols: cfg.cols,
                    rows: cfg.rows,
                    framerate: cfg.framerate,
                    cell_width_px: cfg.cell_width_px,
                    cell_height_px: cfg.cell_height_px,
                    frame_style: cfg.frame_style,
                    keyframe_interval: cfg.framerate * 5,
                },
                output,
            )
            .context("spawning json renderer")?;
            Ok(RendererHandle {
                tx: h.tx.clone(),
                join: RendererJoin::Json(h),
            })
        }
    }
}

pub fn render_recording(rec: &Recording, backend: RendererBackend, output: PathBuf) -> Result<()> {
    let non_json_backend = match backend {
        RendererBackend::Json => return render_json::render_json(rec, &output),
        backend => backend,
    };
    let renderer = spawn_renderer(
        RendererConfig {
            cols: rec.cols,
            rows: rec.rows,
            framerate: rec.framerate,
            cell_width_px: rec.cell_width_px,
            cell_height_px: rec.cell_height_px,
            frame_style: rec.frame_style,
        },
        non_json_backend,
        output,
    )?;

    for i in 0..rec.frames.len() {
        let frame = rec
            .reconstruct(i)
            .ok_or_else(|| anyhow!("failed to reconstruct frame {i}"))?;
        if renderer.tx.send(frame).is_err() {
            break;
        }
    }

    renderer.join()
}
