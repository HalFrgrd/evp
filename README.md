# evp

> **e**mbedded **v**ideo for the terminal **p**rompt — a small Rust CLI
> that ingests [VHS](https://github.com/charmbracelet/vhs)-format scripts
> and produces animated GIFs (and, soon, SVGs) using
> [libghostty](https://ghostty.org) as the underlying terminal emulator.

`evp` runs a real shell inside an embedded Ghostty VT, schedules typed
input from your `.tape` script onto an absolute timeline, snapshots the
terminal at the configured framerate on a worker thread, then renders
the resulting `Recording` to an animated artifact.

| | |
| --- | --- |
| **Architecture deep-dive** | [architecture.md](architecture.md) |
| **Examples** | [examples/](examples/) |
| **Docker image** | `ghcr.io/halfrgrd/evp:latest` |
| **Pre-rendered example GIFs** | `assets` release (linked below) |

## Examples

These GIFs are produced by the [`examples`](.github/workflows/examples.yml)
workflow on every push to `main` and uploaded as assets on the rolling
[`assets` release](https://github.com/HalFrgrd/evp/releases/tag/assets).

### `hello` — bare-minimum recording

[![hello.gif](https://github.com/HalFrgrd/evp/releases/download/assets/hello.gif)](examples/hello.tape)

### `shell-tour` — multi-command session paced with `Wait`

[![shell-tour.gif](https://github.com/HalFrgrd/evp/releases/download/assets/shell-tour.gif)](examples/shell-tour.tape)

### `keys` — modifiers + line editing

[![keys.gif](https://github.com/HalFrgrd/evp/releases/download/assets/keys.gif)](examples/keys.tape)

### `colors` — ANSI SGR colour table

[![colors.gif](https://github.com/HalFrgrd/evp/releases/download/assets/colors.gif)](examples/colors.tape)

### `progress` — in-place line rewrites

[![progress.gif](https://github.com/HalFrgrd/evp/releases/download/assets/progress.gif)](examples/progress.tape)

## Quick start

### Using the Docker image

The published image is fully self-contained — `libghostty-vt.a` is
statically linked into the binary, so there is nothing to install on
your host.

```bash
docker run --rm -v "$PWD:/work" \
    ghcr.io/halfrgrd/evp:latest \
    examples/hello.tape -o hello.gif
```

### Using the binary

You will need:

- a Rust toolchain,
- [Zig 0.15.x](https://ziglang.org/download/) on `$PATH` (libghostty's
  build system).

Both `libghostty-vt` and `libghostty-vt-sys` are pulled directly from
the upstream git repo by Cargo — there's no sibling repo to clone.

```bash
cargo build --release
./target/release/evp examples/hello.tape -o hello.gif
```

The runtime binary has **no** dynamic dependency on `libghostty-vt.so`:

```bash
$ ldd ./target/release/evp | grep ghostty || echo "statically linked"
statically linked
```

### As a library

`evp` ships both a `[lib]` and a `[[bin]]`. The library exposes the full
pipeline so other Rust tools can embed it:

```rust
let script = evp::parse_script(include_str!("../examples/hello.tape"))?;
let out    = evp::run(&script)?;
let json   = evp::recording_to_json(&out.recording)?;
evp::render_gif(&out.recording, &evp::RenderOptions {
    font_path: None,
    font_size: 20.0,
    padding_px: 40,
}, std::path::Path::new("hello.gif"))?;
```

The integration tests in [tests/recording_json.rs](tests/recording_json.rs)
use this same API end-to-end.

## CLI

```text
evp <script> [-o <output.gif>] [--font <path.ttf>] [--recording-json <path.json>]
```

| Flag | Meaning |
| --- | --- |
| `<script>` | Path to a `.tape` file. |
| `-o`, `--output` | Override the script's `Output` directive. Output extension picks the renderer (`.gif` or `.svg`). |
| `--font` | Path to a TTF/OTF used by the GIF renderer. Defaults to a system monospace font discovered via `fontdb`. |
| `--recording-json` | Also dump the intermediate `Recording` to JSON for later re-rendering or inspection. |

## Using `evp` in GitHub Actions

The Docker image is the easiest path:

```yaml
- name: Render terminal demo
  run: |
    docker run --rm -v "$PWD:/work" \
      ghcr.io/halfrgrd/evp:latest \
      docs/demo.tape --output docs/demo.gif
- name: Commit the gif
  uses: stefanzweifel/git-auto-commit-action@v5
  with:
    file_pattern: docs/demo.gif
```

## Status

| Feature | State |
| --- | --- |
| `.tape` parsing (Set / Type / Sleep / Wait / Hide / Show / Ctrl+X / Output / Env) | working |
| PTY-backed shell, libghostty VT, diff-encoded `Recording` | working |
| GIF renderer | working |
| JSON serialisation of `Recording` | working |
| Animated SVG renderer | working (selectable text, ~10× smaller than GIF) |
| `Screenshot` PNG export | TODO |
| Theme support, `Source`, `Copy` / `Paste` | parsed, no-op |

See [architecture.md](architecture.md) for the design rationale.
