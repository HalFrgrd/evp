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

## Main runner loop (detailed)

The loop in [src/runner.rs](src/runner.rs) is deadline-driven rather than
"tick and sleep", which keeps both timing and interactivity stable:

1. **One-time setup**
   - Build PTY + child shell and connect libghostty's write callback to PTY
     writes.
   - Construct a complete absolute event timeline (`build_timeline`), where
     relative script durations are already expanded/scaled.
   - Start the encoder thread and allocate render iterators/scratch state.

2. **Per-iteration PTY drain**
   - `pty.drain_into(&mut terminal)` feeds all currently available shell bytes
     into libghostty's parser so terminal state stays current before decisions
     are made.

3. **Wait-state resolution**
   - If a `Wait` is active, regex matching runs against either the screen or
     last line (depending on `WaitScope`).
   - On match: unblock timeline progression.
   - On timeout: log warning and unblock (matching vhs's pragmatic behaviour).

4. **Event dispatch**
   - While no wait is active and the next scheduled event is due, execute it.
   - `Type`/`Key` writes go to PTY; `Hide`/`Show` toggles a local hidden gate;
     `Screenshot` is currently accepted in parsing but not yet emitted as a
     PNG artifact.
   - If hidden, all events except `Show` are skipped.

5. **Frame capture**
   - While frame deadline(s) are due, capture via ghostty render iterators into
     a dense `RawFrame` (`cells + cursor + default colors + t_ms`).
   - Send each frame over a bounded channel to the encoder thread.
     Backpressure is intentional: if encoding falls behind, capture naturally
     slows rather than dropping frames.

6. **Exit condition**
   - Stop only after the event timeline is exhausted, no wait is active, and
     frame capture has passed the computed `total_duration`.
   - `total_duration` extends beyond the last event by multiple frame intervals
     so the final terminal state is visible in output.

7. **Sleep strategy**
   - Compute the next relevant deadline (next event / frame / wait timeout).
   - `poll(2)` the PTY fd until that deadline so incoming shell output wakes
     the loop immediately.

This design gives deterministic frame timestamps while still reacting quickly
to shell output-driven prompts.

## Rendering internals (GIF and SVG)

Both renderers consume the same diff-compressed `Recording` and call
`Recording::reconstruct(i)` to materialize dense frames as needed. That keeps
the storage format compact while preserving exact rendering fidelity.

### GIF path ([src/render.rs](src/render.rs))

1. **Font + geometry**
   - Load explicit `--font` path or discover a system monospace via `fontdb`.
   - Compute cell metrics from glyph advances (`M` width + scaled height).

2. **Rasterization**
   - Build an RGB canvas per frame.
   - Paint default background, then per-cell background overrides.
   - Draw text glyph outlines with alpha blending (`ab_glyph`).
   - Apply style flags:
     - inverse: swap fg/bg,
     - bold: second draw pass with +1px x offset,
     - underline: 1px line near cell bottom.
   - Cursor is rendered by inverting the covered cell rectangle.

3. **Frame timing + write**
   - Skip visually identical frames to reduce output size.
   - Convert `t_ms` deltas to GIF centiseconds (clamped to viewer-safe minimum).
   - Emit frames via `gif::Encoder` with `Repeat::Infinite` (loop forever).

### SVG path ([src/render_svg.rs](src/render_svg.rs))

1. **Frame windowing**
   - Reconstruct frames and collapse consecutive visually identical frames into
     visibility windows (`start_ms..end_ms`) instead of duplicating markup.

2. **Document model**
   - Static canvas background `<rect>`.
   - One hidden `<g>` per unique frame containing:
     - background run rectangles,
     - text runs (coalesced by style/color),
     - cursor block.

3. **Animation model (SMIL)**
   - A dummy master timer `<animate id="t" ... repeatCount="indefinite"/>`.
   - Each frame group uses `<set attributeName="visibility" ...>` with
     begin/end offsets relative to `t.begin`.
   - When the master timer repeats, visibility sets re-fire, so playback loops
     forever without JavaScript.

4. **Tradeoffs vs GIF**
   - Pros: selectable/searchable text, crisp scaling, often much smaller files.
   - Cons: depends on browser font availability and SMIL support.

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

