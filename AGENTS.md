# AGENTS

Notes for coding agents (and humans) working in `evp`. Keep this short — it
should help you avoid stepping on the same rakes we already found.

## Build / run

- Workspace member of a multi-root setup; `libghostty-rs` and `vhs` live
  alongside it but `evp` builds standalone via `cargo build`.
- Release build is required for any timing or smoke testing — debug is
  ~15-20× slower because of glyph rasterization and gifski quantization.
  - Debug: ~200ms/frame
  - Release: ~14ms/frame
- Smoke test: `./target/release/evp ./examples/hello.tape --output /tmp/x.gif`
- Trace logs: append `--log-level trace` (very chatty).

## Benchmark conventions

`examples/benchmark_render.rs` is the canonical timing harness. Notable
choices, do **not** silently change them:

- **`Set TypingSpeed 80ms`** — chosen so per-character `Type` events land
  on distinct frame deadlines at 30 fps (one keystroke ≈ 2-3 frames).
  Lowering this makes the benchmark dominated by glyph layout instead of
  rendering throughput; raising it stretches the recording for no useful
  signal.
- **`Set Framerate 30`** — matches typical demo output and keeps frame
  count predictable for diff-frame ratios.
- **`RENDER_REPEATS = 4`** — renders the same `Recording` four times so
  the per-pass average filters out cold-cache noise. Pass 1 is almost
  always slower (font load, allocator warmup).
- The benchmark builds its own filesystem fixture under `/tmp/evp-bench-fs-*`
  and writes a `.tape` next to it. Old fixtures are not cleaned up — they're
  small and useful for rerunning by hand.

Typical numbers on a modern x86_64 release build:

```
recording_build_ms ≈ 10000   (capture is bound by the 80ms TypingSpeed)
render_avg_ms      ≈ 1200    (gifski streaming, 320 frames)
```

## Streaming render pipeline (must-know)

`evp` runs three cooperating threads:

1. **PTY/runner** — drives libghostty + the script timeline. Captures one
   `RawFrame` per frame deadline.
2. **Encoder** — folds each `RawFrame` into the diff-compressed
   `Recording` and forwards a clone to the renderer through the
   *frame_tap* channel.
3. **Renderer worker** (gif or svg) — rasterizes/encodes incrementally as
   frames arrive, writing the output file when its receiver closes.

### Hard rules

- The PTY thread **must never block**. It uses `try_send` on the encoder
  channel; if the queue (4096 frames) is full, the frame is dropped and a
  `dropped_capture_frames` warning is logged at the end. Do not switch
  this back to `send()`.
- The encoder uses `try_send` on the frame_tap for the same reason — a
  slow renderer must not stall the recording.
- Bounded channel capacity is `4096` for both `runner→encoder` and
  `encoder→renderer`. This is large enough to absorb bursts on cold
  starts; do not shrink it without a benchmark.

### Sender-drop discipline (the hang we keep re-creating)

Every channel has multiple senders by design (`tx.clone()` is handed to
the runner so it can feed frames). A worker exits its `rx.recv()` loop
**only when all senders drop**. If you hold onto a clone past the
`join()` call, the worker blocks forever and `JoinHandle::join` blocks
with it.

Concretely, in `src/renderer.rs::RendererHandle::join`:

```rust
let RendererHandle { tx, join } = self;
drop(tx);                  // drop the clone we exposed to the runner
match join { … h.join() }  // now the worker can exit
```

Do the same pattern in any new handle wrapper. This was the cause of the
"binary never finishes" bug after the unified renderer refactor — the
worker had two live senders (`self.tx` on the wrapper + the inner
handle's `tx`) and `h.join()` deadlocked.

### Gifski-specific rules (`src/render.rs`)

- `gifski::new()` returns `(collector, writer)`. The writer's `write()`
  call **blocks until the collector is dropped**. Two valid topologies:
  1. Writer on its own thread; main thread drops the collector when the
     frame loop ends, then joins the writer. This is what we use.
  2. Writer on the same thread, run *after* dropping the collector
     (gifski's own example). Easier to reason about but serialises the
     final frame batch.
  Do not call `writer.write()` on the same thread as the collector loop
  unless you have already dropped the collector — instant deadlock.
- `add_frame_rgba` blocks if gifski's internal queue is saturated. That's
  fine because the frame loop owns the collector — no upstream channel
  is held while we wait.

## Threading invariants

- libghostty types (`Terminal`, render iterators) are `!Send + !Sync`. They
  stay on the runner thread. Only owned `RawFrame` values (plain `Vec` +
  cursor + colors) ever cross a thread boundary.
- The encoder thread owns the only mutable `Recording` until it joins.
- Any new background work (e.g. JSON dumping, screenshot side outputs)
  should consume the recording **after** the runner returns it, not
  in parallel — otherwise reconstruction/diff invariants get tricky.

## Output formats

- `.gif` and `.svg` go through the same `renderer::run` entry point.
  Both end up in a streaming worker driven by the `frame_tap` channel.
- Adding a new output format means: implement a `spawn_X_stream`
  returning a handle with `tx: Sender<RawFrame>` + `join() -> Result<()>`,
  then add a `RendererBackend::X` arm in `src/renderer.rs`.

## Debug log breadcrumbs

Useful filters when chasing pipeline bugs:

```
RUST_LOG=evp::runner=debug,evp::encoder=info,evp::render=info ./target/release/evp …
```

Look for these milestones (in order):

```
spawning pty
applied default Snazzy color palette
recording captured frames=…
output written path=…
```

If "recording captured" prints but "output written" never does, you're in
a renderer-thread deadlock — re-read the *Sender-drop discipline*
section.

## Things that look like bugs but aren't

- Empty diff frames (`Frame::Diff` with no `changes`) are intentional;
  they keep the timeline aligned with the framerate so cursor blink and
  default-color changes still animate.
- The recording extends `total_duration` by ~4 frame intervals past the
  last script event. This is so the final terminal state is always
  visible in the output and is not a stuck loop.
- Pass 1 of `benchmark_render` is consistently slower than passes 2-4.
  Font loading + allocator warmup. That's why we average across 4 passes.

## Docker build environment image

- Dockerfiles are split under `docker/` and connected via `docker-bake.hcl`
  `contexts` (`build-env -> builder -> test/runtime/torture/extract-binary`).
- The reusable Rust+Zig image is `docker/build-env.Dockerfile` and is published
  by `.github/workflows/build-env.yml` to `ghcr.io/<owner>/evp-build-env`.
- If an environment cannot resolve `ziglang.org`, point bake at the published
  image instead of rebuilding Zig:
  `docker buildx bake test --set builder.args.BUILD_ENV_IMAGE=ghcr.io/<owner>/evp-build-env:latest`.
