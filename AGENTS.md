# AGENTS

Notes for coding agents (and humans) working in `evp`. Keep this short — it should help you avoid stepping on the same rakes we already found.

## Build & Run Invariants

- **Musl Target**: This environment compiles for the `x86_64-unknown-linux-musl` target by default. When calling the compiled binary directly, make sure to use `./target/x86_64-unknown-linux-musl/...` rather than the standard `./target/...` directories.
- **Standalone Build**: `evp` is a workspace member of a multi-root setup (alongside `libghostty-rs` and `vhs`) but builds standalone via `cargo build`.
- **Prebuilt Ghostty**: For Copilot/restricted builds, **always** use the prebuilt libghostty pkg-config artifact in `assets/libghostty`; do not rely on vendored Ghostty fetches from `libghostty-vt-sys`. Keep `GHOSTTY_SOURCE_DIR` unset.
- **Refresh Artifact**: Refresh the prebuilt artifact using `docker buildx bake extract-libghostty`.
- **Performance Profile**: A release build is required for timing or smoke testing — debug is 15-20× slower because of glyph rasterization and gifski quantization:
  - Debug: ~200ms/frame
  - Release: ~14ms/frame
- **Smoke test**: `./target/x86_64-unknown-linux-musl/release/evp ./examples/hello.tape --output /tmp/x.gif`
- **Trace logs**: Append `--log-level trace` (very chatty).
- **Code Formatting**: Remember to run `cargo fmt` after making any edits to Rust code files.

## Unified Font Architecture (`src/font.rs`)

- **Centralized Source of Truth**: All font loading, glyph fallback selection, and cell metrics calculations are centralized in `src/font.rs`. Avoid ad-hoc font parsing or loading.
- **Lazy Decompression**: Embedded WOFF2 files (JetBrains Mono Nerd Font Mono variants, Noto Sans Mono, Noto Sans Symbols 2, CJK JP subset, Unifont Upper/CSUR) are lazily decompressed to TTF once at runtime using `OnceLock`, caching the decoded TTF bytes.
- **FontSet**: Holds loaded fonts and ordered style-specific indices. When looking up a glyph, `FontSet::select_for_char` walks the list to find the first face covering the character, falling back to the primary regular font if none do.
- **Script Settings**: Font selection is configured strictly via the `.tape` settings (e.g. `Set Font ...`). The `--font` CLI option has been removed.

## SVG Font Embedding (`src/render_svg.rs`)

- **Subsetting is Mandatory**: We use the `font-subset` crate to create WOFF2 subsets of all required fonts. Only the glyphs actually used in the recording are embedded. If subsetting fails, the renderer returns an error.
- **WOFF2 base64 Data**: Embedded fonts are always base64-encoded WOFF2 strings in `@font-face` blocks.
- **Conditional Embedding**: The style block dynamically determines which font variants (bold, italic, CJK fallbacks, etc.) are actually required by scanning the character sets used in the recording. The default font is only embedded if it is actually used.

## Streaming Render Pipeline (Must-Know)

`evp` runs the runner plus one raw-frame consumer worker per output or library recording request:
1. **PTY/runner** — drives libghostty + the script timeline. Captures one `RawFrame` per frame deadline and hands clones to consumers with non-blocking sends.
2. **RawFrameConsumer worker** — gif/svg/json renderers write output files; the optional `FullRecording` consumer builds an in-memory `Recording`.

### Hard Rules

- **PTY Thread Must Never Block**: The PTY thread uses `try_send` on each consumer channel. If a queue is full, the frame is dropped and `raw_frame_consumer_dropped_frames` is logged at the end.
- **Channel Capacity**: Bounded channel capacity is `4096` to absorb startup bursts.
- **Sender-Drop Discipline (The Deadlock Gotcha)**: Every channel has multiple senders (`tx.clone()`). A worker exits its `rx.recv()` loop **only when all senders drop**. If you hold onto a clone past the `join()` call, the worker blocks forever and `JoinHandle::join` deadlocks.
  
  Concretely:
  ```rust
  let RendererHandle { tx, join } = self;
  drop(tx);                  // Drop the clone we exposed to the runner
  match join { ... h.join() } // Now the worker can exit safely
  ```

- **Gifski rules (`src/render_gif.rs`)**: `gifski::new()` returns `(collector, writer)`. The writer's `write()` blocks until the collector is dropped. Never call `writer.write()` on the same thread as the collector loop unless you've already dropped the collector, or you will deadlock instantly.

## Threading Invariants

- libghostty types (`Terminal`, render iterators) are `!Send + !Sync`. They stay on the runner thread. Only owned `RawFrame` values (plain `Vec` + cursor + colors) cross thread boundaries.
- The runner does not own a `RecordingBuilder`. Only the optional `FullRecording` consumer builds an in-memory `Recording`.

## Benchmark Conventions

`examples/benchmark_render.rs` is the canonical timing harness. Do not silently change these settings:
- **`Set TypingSpeed 80ms`** — chosen so per-character `Type` events land on distinct frame deadlines at 30 fps (one keystroke ≈ 2-3 frames).
- **`Set Framerate 30`** — matches typical demo output.
- **`RENDER_REPEATS = 4`** — renders the same `Recording` 4 times to filter out cold-cache noise (Pass 1 font loads, allocator warmups).
- Benchmarks build their filesystem fixture under `/tmp/evp-bench-fs-*` and leave them for easy manual rerunning.

## Debug Log Breadcrumbs

Filter logs with:
```bash
RUST_LOG=evp::runner=debug,evp::render_gif=info,evp::render_svg=info ./target/x86_64-unknown-linux-musl/release/evp ...
```
Look for milestones in order:
`spawning pty` -> `applied terminal theme theme=...` -> `render thread finished path=...` -> `frames captured`.
If `render thread finished` doesn't print, it is a renderer-thread deadlock.

## Things That Look Like Bugs But Aren't

- **Empty Diff Frames**: `Frame::Diff` with no `changes` is intentional to keep the timeline aligned with the framerate for animations like cursor blinking.
- **Padding**: The recording extends `total_duration` by ~4 frame intervals past the last script event to keep the final state visible and avoid stuck loops.
- **Pass 1 Slowness**: The first pass of `benchmark_render` is slower due to font loading and allocator warmup.

## Docker Build Environment

- `ci/libghostty-pkgconfig.Dockerfile` builds the prebuilt `libghostty-vt` static library, headers, and pkg-config files into `assets/libghostty`.
- Run `docker buildx bake extract-libghostty` to refresh `assets/libghostty` so local and CI builds do not need network access for Zig/Ghostty fetches.
- `ci/vhs.Dockerfile` is based on the charmbracelet VHS image and bakes in `scripts/stress_test_program.py` and `scripts/stress_test.tape` for the stress-test comparison run.

## Common Mistakes & Gotchas (From Past Agents)

- **PTY Process Deadlock**: Standard shells (like `dash`/`/bin/sh`) can ignore `SIGHUP` inside virtual PTY/container environments. Always use `SIGKILL` instead of `SIGHUP` to reap child processes in `Child::drop` (`src/pty.rs`), otherwise test suites will deadlock/hang indefinitely.
