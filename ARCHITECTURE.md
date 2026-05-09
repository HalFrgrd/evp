# evp Architecture

`evp` is a small Rust CLI that ingests [VHS](https://github.com/charmbracelet/vhs)–format `.tape`
scripts and produces an animated GIF or SVG by driving a real shell inside an
embedded [libghostty‑vt](https://github.com/ghostty-org/ghostty) terminal
emulator. Capture, diff-encoding, and image encoding all run concurrently on
separate threads.

```
                     try_send                      try_send
   +----------+   bounded(4096)    +-----------+  bounded(4096)  +--------------+
   |  PTY /   | ---RawFrame------> |  encoder  | --RawFrame----> | renderer     |
   |  runner  |                    | (diff +   |   (frame_tap)   | worker       |
   |          | <---PTY bytes--+   |  fold)    |                 | (gif | svg)  |
   +----------+                |   +-----------+                 +--------------+
        ^                      |         |                              |
        |                      |         v                              v
   +-----------------+   +-----------+  Recording (returned)     <output file>
   | libghostty-vt   |   |  shell    |  to caller as RunOutput
   | Terminal        |   +-----------+
   +-----------------+
```

The PTY thread is the timing master. It is **never** blocked on a downstream
channel — both hand-offs use `try_send`, so the worst case under sustained
back-pressure is a dropped frame (logged at the end via `dropped_capture_frames`),
not a stalled terminal.

## Pipeline overview

1. **Parse** the `.tape` source into `Script { settings, env, events, outputs }`
   ([src/script/parser.rs](src/script/parser.rs)). Durations stay relative.
2. **Spawn renderer worker** (only for `run_and_render_*`): a gif or svg
   thread is created up front and handed a `Sender<RawFrame>` it owns.
   See [src/renderer.rs](src/renderer.rs).
3. **Spawn the shell** in a pseudo-terminal ([src/pty.rs](src/pty.rs)) and
   construct a `libghostty_vt::Terminal` with the resolved cell grid
   ([src/runner.rs](src/runner.rs)).
4. **Spawn encoder thread** ([src/encoder.rs](src/encoder.rs)) wired with an
   optional `frame_tap` clone of the renderer's sender.
5. **Schedule** every event onto an absolute `Duration` timeline. `Type "abc"`
   is expanded into N single-char events spaced by `TypingSpeed`; `Down 5`
   into 5 key events spaced by `@delay`; `Sleep` advances the cursor without
   emitting an event.
6. **Run loop** (PTY thread): drain the PTY into the terminal, evaluate
   pending waits, fire all events whose deadline has passed, snapshot the
   terminal at every frame deadline and `try_send` it to the encoder, then
   `poll(2)` on the PTY fd until the next deadline.
7. **Encode** (encoder thread): for each `RawFrame` it (a) `try_send`s a clone
   to the renderer's `frame_tap`, (b) folds it against the previous frame and
   appends a `Frame::Key` or `Frame::Diff` to the in-progress `Recording`.
8. **Render** (renderer thread): rasterizes/encodes incrementally and writes
   the output file when its receiver closes.

## Main runner loop (detailed)

The loop in [src/runner.rs](src/runner.rs) is deadline-driven rather than
"tick and sleep", which keeps both timing and interactivity stable:

1. **One-time setup**
   - Build PTY + child shell and connect libghostty's write callback to PTY
     writes.
   - Construct a complete absolute event timeline (`build_timeline`), where
     relative script durations are already expanded/scaled.
   - Start the encoder thread with the (optional) renderer `frame_tap`.

2. **Per-iteration PTY drain**
   - `pty.drain_into(&mut terminal)` feeds all currently available shell bytes
     into libghostty's parser so terminal state stays current before decisions
     are made.

3. **Wait-state resolution**
   - If a `Wait` is active, regex matching runs against either the screen or
     last line (depending on `WaitScope`).
   - On match: unblock timeline progression. On timeout: log warning and
     unblock (matching vhs's pragmatic behaviour).

4. **Event dispatch**
   - While no wait is active and the next scheduled event is due, execute it.
   - `Type`/`Key` writes go to PTY; `Hide`/`Show` toggles a local hidden gate.
   - If hidden, all events except `Show` are skipped.

5. **Frame capture**
   - While frame deadlines are due, capture via ghostty render iterators into
     a dense `RawFrame` (cells + cursor + default colors + `t_ms`).
   - **Non-blocking** `encoder.tx.try_send(frame)`. On `Full`, increment
     `dropped_capture_frames` and continue — the runner never stalls.

6. **Exit condition**
   - Stop only after the event timeline is exhausted, no wait is active, and
     frame capture has passed `total_duration` (last event + ~4 frame
     intervals so the final state is always captured).

7. **Sleep strategy**
   - Compute the next deadline (next event / frame / wait timeout) and
     `poll(2)` the PTY fd until then so shell output wakes us early.

## Three-thread streaming pipeline

| Thread        | Owns                                                         | Responsibility                                                                |
|---------------|--------------------------------------------------------------|-------------------------------------------------------------------------------|
| **PTY/runner**| `Terminal`, `Pty`, `KeyTranslator`, capture iterators        | Drive the VT, pump the script timeline, capture frames at the framerate.     |
| **encoder**   | `Recording` accumulator, `frame_tap` clone (if any)          | Diff every frame against the previous, forward a clone to the renderer.      |
| **renderer**  | gif/svg encoder state, output file                           | Rasterize and write incrementally as frames arrive.                          |

### Channels

Both pipeline channels are bounded `crossbeam_channel`s with capacity `4096`
([src/encoder.rs](src/encoder.rs), [src/render.rs](src/render.rs)):

- `runner -> encoder`: PTY thread `try_send`s frames; full = drop + log.
- `encoder -> renderer` (frame_tap): encoder `try_send`s clones; full or
  disconnected = drop the forward, recording still completes.

The recording is **always** built completely (good for JSON dumps) even if
the renderer falls behind and some frames don't make it into the rendered
output. In practice the renderer keeps up easily on release builds.

### Sender-drop discipline (deadlock avoidance)

The renderer worker exits its `rx.recv()` loop only when **all** senders
drop. `RendererHandle` exposes a `tx: Sender<RawFrame>` clone for the runner;
it must be dropped *before* `JoinHandle::join` is called, otherwise the
worker can never finish:

```rust
// src/renderer.rs
let RendererHandle { tx, join } = self;
drop(tx);                        // drop the clone exposed to the runner
match join { RendererJoin::Gif(h) => h.join(), … }
```

Gifski has the same shape internally: `writer.write()` blocks until the
collector is dropped, so the gif worker spawns a writer thread, runs the
collector loop, drops the collector, then joins the writer.

### libghostty thread safety

libghostty objects (`Terminal`, render iterators) are `!Send + !Sync` and
never cross thread boundaries. Only owned `RawFrame` values (plain
`Vec<CellSnap>` + cursor state) move between threads.

## Public API surfaces

The library exposes both a "capture only" path and "capture + stream render"
paths:

- `evp::run(&Script) -> RunOutput` — drives the script, returns the
  `Recording`. No image encoding.
- `evp::run_and_render_gif(&Script, RenderOptions, PathBuf)` — three-thread
  pipeline; writes the gif as it goes.
- `evp::run_and_render_svg(&Script, SvgOptions, PathBuf)` — same shape with
  the SVG worker.
- `evp::render_gif(&Recording, &RenderOptions, &Path)` /
  `evp::render_svg(&Recording, &SvgOptions, &Path)` — render an existing
  in-memory `Recording`. Internally these still use the streaming worker; the
  caller just feeds reconstructed frames synchronously.

The CLI (`src/bin/evp.rs`) dispatches purely on the output extension and
calls one of the streaming entry points.

## Rendering internals

Both renderers consume `RawFrame`s through the `frame_tap` channel. They
never touch the `Recording` directly during a streaming run.

### GIF path ([src/render.rs](src/render.rs))

1. **Font + geometry**
   - Load explicit `--font` path or fall back to the embedded JetBrainsMono
     family (regular/bold/italic/bold-italic).
   - Compute cell metrics from glyph advances (`M` width + scaled height).

2. **Streaming worker**
   - `gifski::new(Settings { … })` returns `(collector, writer)`.
   - Spawn a writer thread that runs `writer.write(file, NoProgress)`.
   - In the worker thread, loop on `rx.recv()`:
     - rasterize the frame into RGBA,
     - skip if the buffer matches the previous (visually identical) frame,
     - convert `t_ms` deltas to GIF centiseconds (clamped to viewer-safe
       minimum),
     - `collector.add_frame_rgba(idx, ImgVec, delay_seconds)`.
   - When the channel closes, `drop(collector)` (signals EOF to gifski) and
     join the writer thread. The writer's `write()` was already mid-flight
     and now finishes the file.

3. **Rasterization details**
   - Build an RGB canvas per frame.
   - Paint default background, then per-cell background overrides.
   - Draw text glyph outlines with alpha blending (`ab_glyph`), choosing
     regular/bold/italic/bold-italic faces by style flag.
   - Apply style flags: inverse swaps fg/bg, underline draws a 1px line near
     cell bottom, bold uses the bold face when available else a +1px x
     offset second pass.
   - Cursor inverts the covered cell rectangle.

### SVG path ([src/render_svg.rs](src/render_svg.rs))

1. **Streaming worker**
   - Mirrors the gif worker: bounded channel in, output file out.
   - Frames are reconstructed/coalesced incrementally as they arrive — no
     full `Recording` materialisation needed.

2. **Document model**
   - Static canvas background `<rect>`.
   - One hidden `<g>` per unique frame containing background run rectangles,
     coalesced text runs, and a cursor block.

3. **Animation model (SMIL)**
   - A dummy master timer `<animate id="t" repeatCount="indefinite"/>`.
   - Each frame `<g>` uses `<set attributeName="visibility">` with begin/end
     offsets relative to `t.begin`. Loops forever without JavaScript.

4. **Tradeoffs vs GIF**
   - Pros: selectable/searchable text, crisp scaling, often much smaller.
   - Cons: depends on browser font availability and SMIL support.

## Recording format

`Recording` is plain Rust data with `serde` derives, so it round-trips
cleanly to JSON:

```rust
struct Recording {
    cols: u16, rows: u16, framerate: u32,
    cell_width_px: u32, cell_height_px: u32, padding_px: u32,
    frames: Vec<Frame>,
}
enum Frame {
    Key  { t_ms, cursor, default_fg, default_bg, cells: Vec<CellSnap> },
    Diff { t_ms, cursor, default_fg, default_bg, changes: Vec<CellChange> },
}
```

A `CellSnap` is a UTF-8 grapheme cluster + fg/bg + 1 byte of style flags
(bold, italic, underline, inverse, strikethrough). Keyframes are inserted
every `framerate * 5` frames so random-access reconstruction (used by the
non-streaming renderer paths and `Recording::reconstruct(i)`) is bounded.

Empty diff frames (no `changes`) are intentional — they keep the timeline
aligned with the framerate so cursor blink and default-color changes still
animate.

## Key translation

VHS keys are turned into PTY bytes by [src/keys.rs](src/keys.rs), which
wraps libghostty's own `key::Encoder`. The encoder is refreshed from the
terminal's mode state before every press (`set_options_from_terminal`) so
DECCKM, modifyOtherKeys, kitty progressive enhancement etc. are honoured
exactly the same way they would be for an interactive user. Plain `Type`
text bypasses the encoder and is written as raw UTF-8.
