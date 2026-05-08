# evp Architecture

`evp` is a small Rust CLI that ingests [VHS](https://github.com/charmbracelet/vhs)–format `.tape`
scripts and produces an animated GIF by driving a real shell inside an embedded
[libghostty‑vt](https://github.com/ghostty-org/ghostty) terminal emulator.

```
            +------------------+      RawFrame      +-------------------+
 .tape ---> | parse + schedule | ---channel-------> | encoder thread    | --> Recording
            +------------------+                    | (diff compression)|        |
                  ^   |                             +-------------------+        v
                  |   v                                                     +---------+
            +------------------+    bytes      +-----------+                | render  |
            | KeyTranslator    | <----PTY----> |  shell    |                |  GIF    |
            +------------------+               +-----------+                +---------+
                  ^   |
                  |   v
            +------------------+
            |  libghostty-vt   |
            |  (Terminal)      |
            +------------------+
```

## Pipeline overview

1. **Parse** the `.tape` source into `Script { settings, env, events, outputs }`
   ([src/script/parser.rs](src/script/parser.rs)). All durations are kept
   relative at this stage.
2. **Spawn** the shell in a pseudo‑terminal ([src/pty.rs](src/pty.rs)) and
   construct a `libghostty_vt::Terminal` with the resolved cell grid
   ([src/runner.rs](src/runner.rs#L80)).
3. **Schedule** every event onto an absolute `Duration` timeline
   ([src/runner.rs#L213](src/runner.rs)). `Type "abc"` is expanded into N
   single‑char events spaced by `TypingSpeed`; `Down 5` into 5 key events
   spaced by `@delay`; `Sleep` advances the cursor without emitting an event.
4. **Run loop** (main thread): drain the PTY into the terminal, evaluate
   pending waits, fire all events whose deadline has passed, snapshot the
   terminal at every frame deadline, then `poll(2)` on the PTY fd until the
   next deadline (so output wakes us early).
5. **Encode** (worker thread): each `RawFrame` is folded against the
   previous one and emitted as either a `Frame::Key` (full grid) or
   `Frame::Diff` (changed cells only).
6. **Render** the resulting `Recording` into a GIF using `ab_glyph` for
   glyph outlines and the `gif` crate for encoding
   ([src/render.rs](src/render.rs)).

## Threading

Two threads, communicating over a single bounded `crossbeam_channel`:

| Thread | Owns | Responsibility |
|---|---|---|
| **main**    | `Terminal`, `Pty`, `KeyTranslator`, `RenderState`/`RowIterator`/`CellIterator` | Drive the VT, pump the script timeline, capture frames at the framerate. |
| **encoder** | `Recording` accumulator                                                       | Receive `RawFrame`s, diff them against the previous, append to `Recording`. |

libghostty objects are `!Send + !Sync` and never cross thread boundaries —
only owned `RawFrame` values do, which are plain `Vec<CellSnap>` + cursor
state. The GIF renderer runs on the main thread *after* the encoder joins,
so it has uncontended access to the finished `Recording`.

The split exists so the main thread's deadline math is never blocked by GIF
quantisation or JSON serialisation: the only work it does per frame is the
diff‑agnostic cell read.

## Recording format

`Recording` is plain Rust data with `serde` derives, so it round‑trips
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

A `CellSnap` is a UTF‑8 grapheme cluster + fg/bg + 1 byte of style flags
(bold, italic, underline, inverse, strikethrough). Keyframes are inserted
every `framerate * 5` frames so random‑access reconstruction (used by the
renderer) is bounded.

`Recording::reconstruct(i)` walks back to the nearest keyframe and replays
diffs up to `i`, returning a dense `RawFrame`. The renderer uses this to
rasterise each output frame.

## Key translation

VHS keys are turned into PTY bytes by [src/keys.rs](src/keys.rs), which
wraps libghostty's own `key::Encoder`. The encoder is refreshed from the
terminal's mode state before every press
(`set_options_from_terminal`) so DECCKM, modifyOtherKeys, kitty progressive
enhancement etc. are honoured exactly the same way they would be for an
interactive user. Plain `Type` text bypasses the encoder and is written as
raw UTF‑8 — that's what an actual keyboard would emit and avoids per‑char
overhead.

## Is the crossbeam channel large enough?

The channel is `crossbeam_channel::bounded(64)`
([src/encoder.rs#L42](src/encoder.rs)). Concretely:

* **Production rate**: one `RawFrame` per frame deadline, i.e. up to
  `framerate` frames/sec (default 50, common values 30–60).
* **Per‑frame work on the consumer**: a single linear scan of `cols * rows`
  cells comparing the current dense frame to the previous one, plus a
  small `Vec<CellChange>` allocation. For an 80×24 grid (1 920 cells) this
  is on the order of microseconds — orders of magnitude faster than the
  16–33 ms between frames.

So under normal operation the channel sits at depth 0–1 and 64 slots is
massive overkill. The bound matters in two corner cases:

1. **GC / OS hiccup**: if the encoder thread is preempted for, say, 100 ms
   at 50 fps the channel will briefly fill to ~5 entries — well under 64.
2. **Back‑pressure as a safety valve**: if the encoder ever *did* fall
   behind (e.g. someone adds expensive PNG keyframe encoding) `send` blocks
   the main thread. That's the right behaviour: it stalls capture rather
   than silently dropping frames.

A `RawFrame` for a 200×60 grid is roughly `200 * 60 * (~40 bytes) ≈
480 KiB`, so a fully saturated 64‑slot channel peaks at ~30 MiB — fine for
a recording tool but worth knowing if we ever want to record very large
terminals at very high framerates. If that becomes a concern the bound
can be lowered (32 or even 8 is plenty) without behavioural change.

**Verdict: 64 is comfortably enough; the bound is there for safety, not
throughput.**

## Next steps

These are the two highest‑leverage follow‑ups, in order:

### 1. `Screenshot` export

`Screenshot "path.png"` is already parsed and threaded through the
timeline (`Event::Screenshot(path)`) but the runner currently just logs a
warning. The plan:

1. When the runner hits the event, record `(path, t_ms)` into a side
   list it owns.
2. After `runner::run` returns, iterate that list and for each entry:
   * find the frame in the `Recording` whose `t_ms` is closest to the
     screenshot's `t_ms`,
   * call `Recording::reconstruct(i)` to materialise it,
   * call a new `render::rasterize_frame` → `image::RgbImage::from_raw`
     pipeline and write the PNG via the `image` crate (already a
     dependency).

This is small, isolated, and exercises the same reconstruction path the
GIF renderer uses, so it doubles as a regression test for the diff format.

### 2. Animated SVG output

The user explicitly mentioned this as a future target. The architecture
is already set up for it: the `Recording` is the rendering‑agnostic
intermediate. To add SVG:

1. Introduce a `render::Renderer` trait with one method,
   `render(&Recording, &Path) -> Result<()>`, and move the GIF encoder
   behind a `GifRenderer` impl.
2. Add an `SvgRenderer` impl that emits one `<svg>` document containing:
   * a `<style>` block with the cell metrics + default colors,
   * one `<g class="frame fN">` per `Frame`, where each `<g>` either
     re‑emits the full grid (keyframe) or only the changed cells (diff
     frame),
   * CSS animation timing derived from each frame's `t_ms` so that the
     diff structure of the `Recording` maps almost directly onto a tiny,
     scalable, text‑indexable artifact.
3. Pick the renderer from the output extension (`.gif` → GIF, `.svg` →
   SVG) in `main::real_main`.

Because diffs already minimise the changed‑cell set per frame, the SVG
output ends up dramatically smaller than a frame‑per‑frame screenshot
approach — the encoder thread's work directly subsidises the renderer.
