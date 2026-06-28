# evp Architecture

`evp` is a small Rust CLI that ingests [VHS](https://github.com/charmbracelet/vhs)–format `.tape`
scripts and produces GIF, SVG, or JSON outputs by driving a real shell inside an
embedded [libghostty‑vt](https://github.com/ghostty-org/ghostty) terminal
emulator. The runner captures dense `RawFrame`s and hands them to any
configured raw-frame consumers. CLI rendering does not build a full in-memory
recording; the opt-in `FullRecording` consumer exists for library callers that
need one.

## Build prerequisite for sandboxed environments

`evp` expects a prebuilt libghostty pkg-config bundle under
`assets/libghostty` (`lib/`, `include/`, `share/pkgconfig/`). Generate or
refresh it with:

`docker buildx bake extract-libghostty`

Builds intentionally keep `GHOSTTY_SOURCE_DIR` unset so `libghostty-vt-sys`
uses pkg-config rather than trying to fetch and build Ghostty during Cargo
build scripts.

```
                        try_send
   +----------+   bounded(4096)   +---------------------+
   |  PTY /   | ---RawFrame-----> | RawFrameConsumer(s) |
   |  runner  |                   | gif / svg / json /  |
   |          | <---PTY bytes--+  | FullRecording       |
   +----------+                |  +---------------------+
        |                      |            |
        |                      |            v
        |                      |      <output file> or
        |                      |       Recording
        |                      |
   +-----------------+   +-----------+
   | libghostty-vt   |   |  shell    |
   | Terminal        |   +-----------+
   +-----------------+
```

The PTY thread is the timing master. It is **never** blocked on a downstream
channel — every hand-off uses `try_send`, so the worst case under sustained
consumer back-pressure is a dropped consumer frame, not a stalled terminal.

## Pipeline overview

1. **Parse** the `.tape` source into `Script { settings, env, events, outputs }`
   ([src/script/parser.rs](src/script/parser.rs)). Durations stay relative.
2. **Spawn raw-frame consumers** as needed: one renderer thread per output
   ([src/renderer.rs](src/renderer.rs)) and, for library callers that need an
   in-memory recording, one `FullRecording` worker
   ([src/full_recording.rs](src/full_recording.rs)).
3. **Spawn the shell** in a pseudo-terminal ([src/pty.rs](src/pty.rs)) and
   construct a `libghostty_vt::Terminal` with the resolved cell grid
   ([src/runner.rs](src/runner.rs)).
4. **Schedule** every event onto an absolute `Duration` timeline. `Type "abc"`
   is expanded into N single-char events spaced by `TypingSpeed`; `Down 5`
   into 5 key events spaced by `@delay`; `Sleep` advances the cursor without
   emitting an event.
5. **Run loop** (PTY thread): drain the PTY into the terminal, evaluate
   pending waits, fire all events whose deadline has passed, snapshot the
   terminal at every frame deadline, `try_send` clones to raw-frame consumers,
   then `poll(2)` on the PTY fd until the next deadline.
6. **Consume** (consumer threads): each output or library consumer processes
   the runner's dense frames independently and finishes when its receiver
   closes.

## Main runner loop (detailed)

The loop in [src/runner.rs](src/runner.rs) is deadline-driven rather than
"tick and sleep", which keeps both timing and interactivity stable:

1. **One-time setup**
   - Build PTY + child shell and connect libghostty's write callback to PTY
     writes.
   - Construct a complete absolute event timeline (`build_timeline`), where
     relative script durations are already expanded/scaled.
   - Start any requested raw-frame consumers.

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
   - **Non-blocking** `try_send` to each raw-frame consumer. On `Full`,
     increment the consumer dropped-frame counter and continue — the runner
     never stalls on output encoding or recording assembly.

6. **Exit condition**
   - Stop only after the event timeline is exhausted, no wait is active, and
     frame capture has passed `total_duration` (last event + ~4 frame
     intervals so the final state is always captured).

7. **Sleep strategy**
   - Compute the next deadline (next event / frame / wait timeout) and
     `poll(2)` the PTY fd until then so shell output wakes us early.

## Interactive Recording (`evp record`)

The `evp record` subcommand launches an interactive single-paned terminal multiplexer. It lets users run terminal sessions inside a PTY, record their keystrokes/mouse actions directly to a `.tape` file, and simultaneously stream frames to compile a `demo.gif` in real-time.

```
       Crossterm Events
     (Keyboard, Mouse)
            │
            ▼
     +─────────────+              PTY writes (raw chars/translated codes)
     │ evp record  │ ────────────────────────────────────────────────────────► +───────────+
     │             │ ◄──────────────────────────────────────────────────────── |   shell   |
     +─────────────+             PTY reads (stdout/stderr bytes)               +───────────+
        │       │
        │       │  Sends dense RawFrames (with mouse coords)
        │       ▼
        │    +─────────────────────+
        │    │ background renderer │ ────► demo.gif
        │    +─────────────────────+
        ▼
     Ratatui Drawing
   (Status Bar + Grid)
        │
        ▼
     host stdout
```

### 1. Dual Polling Loop
`evp record` drives two concurrent input channels inside its central event loop using `crossbeam_channel::select!`:
* **PTY stdout receiver:** Read bytes coming from the running shell, feed them into `libghostty_vt::Terminal`'s parser to maintain the screen grid state, and trigger a Ratatui host screen redraw.
* **Crossterm event receiver:** Capture keyboard and mouse inputs from the user's host terminal standard input.

### 2. Status Bar and Software Blinking
The host terminal alternates into a full-screen application layout managed by **Ratatui**. The layout is divided into:
* **Header Line:** Displays `"EVP recording active (X seconds), exit the program to stop recording."` prefixed with a green blinking dot `●`. The blinking is driven via software calculations (`elapsed.as_millis() % 1000 < 500`) to guarantee a consistent visual blink across all terminal emulators.
* **Divider:** A horizontal border line built using box-drawing characters.
* **Terminal Body:** Renders the actual cells of the `libghostty-vt` terminal grid using custom Ratatui paragraph styling.

### 3. Mouse Coordinate Translation
Because the status bar and divider shrink the vertical height of the terminal viewport by `2` rows:
* When Crossterm intercepts mouse click/motion coordinates from the host terminal, `evp record` automatically subtracts `2` from the `row` coordinate before encoding and forwarding the event to the PTY.
* For live rendering, the translated mouse coordinate is attached to the captured `RawFrame::mouse_cursor` structure so the background GIF/SVG renderer draws the pointer at the correct cell.
* If no mouse movement or click events occur for `> 3s`, the mouse cursor is automatically set to `None` to hide it from all outputs.

### 4. Mouse Movement Simplification (`MouseSegmentTracker`)
To avoid writing thousands of individual high-frequency coordinates to the final `.tape` script, mouse movements are simplified geometrically:
* **Collinearity validation:** Consecutive movements (`MouseMove`/`MouseDrag` actions) are buffered in a segment. For each new point, a distance formula checks if it lies within a `1.5` cell tolerance of the straight line segment between the start and end coordinates.
* **Segment breaking:** A segment is flushed as a single tape event when collinearity is broken, when a pause of `> 1s` is detected, or when mouse button states change (e.g. click/release). This dramatically simplifies the output script.

### 5. Keyboard Forwarding
* Keyboard inputs captured during the recording session are translated into logical keys and routed through the `KeyTranslator` (which uses `libghostty`'s key encoder) to generate the appropriate byte sequences before writing them to the PTY.

## Streaming pipeline

| Thread        | Owns                                                         | Responsibility                                                                |
|---------------|--------------------------------------------------------------|-------------------------------------------------------------------------------|
| **PTY/runner**| `Terminal`, `Pty`, `KeyTranslator`, capture iterators        | Drive the VT, pump the script timeline, capture raw frames.                  |
| **renderer**  | gif/svg/json encoder state, output file                      | Write one output incrementally as frames arrive.                             |
| **FullRecording** | `RecordingBuilder`                                      | Optional library-only in-memory `Recording` assembly.                        |

### Channels

Raw-frame consumer channels are bounded `crossbeam_channel`s with capacity `4096`
([src/render_common.rs](src/render_common.rs)):

- `runner -> consumer`: PTY thread `try_send`s clones directly to each
  consumer; full or disconnected = drop that consumer forward.

The PTY loop never waits for a slow consumer. In practice the workers keep up
easily on release builds.

### Sender-drop discipline (deadlock avoidance)

Each consumer worker exits its `rx.recv()` loop only when **all** senders drop.
`RendererHandle` exposes a `tx: Sender<RawFrame>` clone for the runner; it must
be dropped *before* `JoinHandle::join` is called, otherwise the worker can never
finish:

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

The library exposes stats-only, recording, and streaming render paths:

- `evp::run(&Script) -> RunStats` — drives the script without attaching any
   consumers.
- `evp::run_and_return_recording(&Script) -> RunOutput` — attaches the
   `FullRecording` consumer and returns an in-memory `Recording`.
- `evp::run_and_render(&Script, Vec<(RendererBackend, PathBuf)>)` — spawns one
   renderer thread per output, streams frames to all of them, and returns
   `RunStats`.
- `evp::run_and_render_gif(&Script, RenderOptions, PathBuf)` — convenience
   wrapper for one GIF output.
- `evp::run_and_render_svg(&Script, SvgOptions, PathBuf)` — convenience
   wrapper for one SVG output.
- `evp::run_and_render_json(&Script, PathBuf)` — convenience wrapper for one
   JSON `Recording` output.
- `evp::render_gif(&Recording, &RenderOptions, &Path)` /
  `evp::render_svg(&Recording, &SvgOptions, &Path)` /
  `evp::render_json(&Recording, &Path)` — render an existing in-memory
  `Recording`.

The CLI (`src/bin/evp.rs`) dispatches on every output extension and calls the
multi-renderer streaming entry point.

## Rendering internals

All streaming renderers consume `RawFrame`s directly from the runner. GIF and
SVG never touch a full `Recording` during a streaming run; JSON builds the same
intermediate `Recording` format in its own consumer thread because JSON output
is that recording.

### GIF path ([src/render_gif.rs](src/render_gif.rs))

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

### JSON path ([src/render_json.rs](src/render_json.rs))

The JSON backend is a first-class renderer selected by `.json` outputs (e.g. via `--output demo.json`). It consumes dense `RawFrame`s on its own thread, folds them
with the shared `RecordingBuilder`, then writes the pretty-printed
intermediate `Recording` JSON when the runner closes the channel.

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
