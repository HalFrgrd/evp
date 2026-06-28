use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::Sender;

use crate::{
    RawFrame, Recording, RenderOptions, SvgOptions,
    render_gif::{self, GifStreamHandle},
    render_json::{self, JsonStreamHandle},
    render_svg::{self, SvgStreamHandle},
};

pub use crate::render_common::ViewportConfig;

pub enum RendererBackend {
    Gif(RenderOptions),
    Svg(SvgOptions),
    Json,
}

impl RendererBackend {
    pub fn for_path(
        path: &std::path::Path,
        render_opts: &RenderOptions,
        embed_fonts: bool,
        no_system_fonts: bool,
    ) -> Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz") {
            Ok(RendererBackend::Svg(SvgOptions {
                font_path: render_opts.font_path.clone(),
                font_size: render_opts.font_size,
                embed_fonts,
                no_system_fonts,
                ..Default::default()
            }))
        } else if ext.eq_ignore_ascii_case("json") {
            Ok(RendererBackend::Json)
        } else {
            let mut r_opts = render_opts.clone();
            r_opts.no_system_fonts = no_system_fonts;
            Ok(RendererBackend::Gif(r_opts))
        }
    }
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
    cfg: ViewportConfig,
    backend: RendererBackend,
    output: PathBuf,
) -> Result<RendererHandle> {
    match backend {
        RendererBackend::Gif(opts) => {
            let h =
                render_gif::spawn_gif_stream(cfg, opts, output).context("spawning gif renderer")?;
            Ok(RendererHandle {
                tx: h.tx.clone(),
                join: RendererJoin::Gif(h),
            })
        }
        RendererBackend::Svg(opts) => {
            let h =
                render_svg::spawn_svg_stream(cfg, opts, output).context("spawning svg renderer")?;
            Ok(RendererHandle {
                tx: h.tx.clone(),
                join: RendererJoin::Svg(h),
            })
        }
        RendererBackend::Json => {
            let h = render_json::spawn_json_stream(cfg, cfg.framerate * 5, output)
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
        ViewportConfig::new(
            rec.cols,
            rec.rows,
            rec.framerate,
            rec.cell_width_px,
            rec.cell_height_px,
            rec.frame_style,
            rec.font_size_px,
            rec.char_height_px,
            rec.ascent_px,
            rec.letter_spacing,
        ),
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
