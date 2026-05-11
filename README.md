# evp

> **evp** — a small Rust CLI
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
workflow on every push to `master` and uploaded as assets on the rolling
[`assets` release](https://github.com/HalFrgrd/evp/releases/tag/assets).

### `evp_demo` — running evp on the command line

[![evp_demo.gif](https://github.com/HalFrgrd/evp/releases/download/assets/evp_demo.gif)](examples/evp_demo.tape)

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

Grab a prebuilt binary into the current directory — no Rust, no Zig,
no Docker:

```bash
curl -sSfL https://raw.githubusercontent.com/HalFrgrd/evp/master/install.sh | sh
```

This drops an `evp` executable into `$PWD`. Override the destination
with `EVP_INSTALL_DIR=~/.local/bin sh` or pin a release with
`EVP_VERSION=v0.2.0 sh`. The binary is a single fully static
(`musl` + `static-pie`) ELF — see [System requirements](#system-requirements)
below.

Verify the install with the built-in demo (no script files needed):

```bash
./evp --run-test-script
# → writes ./evp-test.gif
```

Then render your own scripts:

```bash
./evp examples/hello.tape -o hello.gif
```

### System requirements

The prebuilt binary is statically linked against `musl` libc as a
`static-pie` executable, so it has **no** dynamic library dependencies
at all:

```text
$ ldd evp
        statically linked
$ file evp
evp: ELF 64-bit LSB pie executable, x86-64, ..., static-pie linked
```

It runs unmodified on any x86_64 Linux kernel — Alpine, Debian (any
version), Ubuntu, RHEL/CentOS (any version), distroless, scratch — no
glibc version requirement.

**Not required**: fontconfig, freetype, ImageMagick, ffmpeg, a display
server, or any installed fonts. JetBrains Mono is embedded into the
binary.

### Using the Docker image

If you don't want a binary on the host, the published image is fully
self-contained:

```bash
docker run --rm -v "$PWD:/work" \
    ghcr.io/halfrgrd/evp:latest \
    examples/hello.tape -o hello.gif
```

### Build from source

Build dependencies (only needed at *build* time — the resulting binary
does not need either):

- a Rust toolchain,
- [Zig 0.15.x](https://ziglang.org/download/) on `$PATH` (libghostty's
  build system).

Both `libghostty-vt` and `libghostty-vt-sys` are pulled directly from
the upstream git repo by Cargo — there's no sibling repo to clone.

```bash
cargo build --release
./target/release/evp examples/hello.tape -o hello.gif
```

The resulting binary has **no** dynamic dependency on `libghostty-vt.so`
and **no** runtime requirement on Zig:

```bash
$ ldd ./target/release/evp | grep ghostty || echo "statically linked"
statically linked
```

#### Reproducible static build via Docker

If you'd rather not install Rust or Zig locally, the project ships a
`docker buildx bake` recipe that produces the same fully-static
`musl` binary that the GitHub release uses. One command:

```bash
docker buildx bake extract-binary
# → /tmp/evp-build/evp  (≈6 MiB, static-pie, stripped)
```

The `extract-binary` target writes the binary directly to `/tmp/evp-build`
on the host via buildx's local output writer — no `docker create` /
`docker cp` plumbing. Other targets in the same bake file:

```bash
docker buildx bake test       # workspace cargo test (CI parity)
docker buildx bake runtime    # builds evp:local container image
docker buildx bake build-env   # Rust+Zig base image
docker buildx bake builder     # intermediate builder image
# If ziglang.org is blocked, reuse a published build-env image:
docker buildx bake test --set builder.args.BUILD_ENV_IMAGE=ghcr.io/<owner>/evp-build-env:latest
```

#### Restricted-network environments (Copilot cloud agent etc.)

A clean checkout needs network access to two places:

- `https://ziglang.org/...` for the Zig 0.15.x toolchain, and
- the upstream Ghostty source + Zig package mirror, fetched lazily
  by `libghostty-vt-sys`'s `build.rs` the first time `zig build`
  runs.

Sandboxed environments (notably the GitHub Copilot cloud agent's
firewall) typically block the Zig hosts. Two ways to cope:

- **Cargo / native builds.** A `.github/workflows/copilot-setup-steps.yml`
  is included that installs Zig and runs `cargo fetch` + `cargo build`
  on a vanilla GitHub-hosted runner — i.e. *before* the agent
  firewall engages — so `~/.cargo` and `target/` are fully warmed
  for the agent. No further changes are required to use the agent
  on this repo.
- **Docker builds.** Use the published Rust+Zig image as shown
  above (`--set builder.args.BUILD_ENV_IMAGE=ghcr.io/<owner>/evp-build-env:latest`).
  Cargo still needs `https://github.com` (allowed by the default
  Copilot firewall) to resolve the `libghostty-rs` git dep.

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
evp <script> [-o <output.gif>] [--font <path.ttf>] [--recording-json <path.json>] [--log-level <level>]
evp --run-test-script [-o <output.gif>]
```

| Flag | Meaning |
| --- | --- |
| `<script>` | Path to a `.tape` file. Required unless `--run-test-script` is set. |
| `--run-test-script` | Run a small built-in demo tape embedded in the binary. Writes `./evp-test.gif` by default. Useful for verifying an install end-to-end with no external files. |
| `-o`, `--output` | Override the script's `Output` directive. Output extension picks the renderer (`.gif` or `.svg`). |
| `--font` | Path to a TTF/OTF/TTC used by the GIF renderer. Defaults to embedded `JetBrains Mono` family files in `assets/fonts/`. |
| `--recording-json` | Also dump the intermediate `Recording` to JSON for later re-rendering or inspection. |
| `--log-level` | Explicit level override: `error`, `warn`, `info`, `debug`, `trace`. |
| `--version` | Print extended build metadata (git SHA/branch/date/dirty flag, build timestamp, rustc, target triple, opt-level). |

`--log-level` overrides the default `info` level. If it is not provided,
`RUST_LOG` is still honored.

The embedded font family is distributed under SIL OFL 1.1; see [licenses/JETBRAINSMONO-OFL-1.1.txt](licenses/JETBRAINSMONO-OFL-1.1.txt).

GIF rendering uses style-specific JetBrains Mono faces when available:

- regular: `JetBrainsMono-Regular.ttf`
- bold: `JetBrainsMono-Bold.ttf`
- italic: `JetBrainsMono-Italic.ttf`
- bold italic: `JetBrainsMono-BoldItalic.ttf`

Additional JetBrains Mono weights are also embedded under `assets/fonts/`.
If a requested style face is unavailable, rendering logs a warning and
falls back to the regular face.

## Using `evp` in GitHub Actions

The recommended path is the bundled composite action, which downloads a
prebuilt linux-amd64 binary from the matching GitHub Release and adds it
to `$PATH`. No Docker daemon, no Rust toolchain, no Zig — just one
`uses:` step.

```yaml
- uses: HalFrgrd/evp@v1
  with:
    script: docs/demo.tape
    output: docs/demo.gif

- uses: stefanzweifel/git-auto-commit-action@v5
  with:
    file_pattern: docs/demo.gif
```

Pin to a specific release for reproducibility:

```yaml
- uses: HalFrgrd/evp@v0.2.0
  with:
    script: docs/demo.tape
    output: docs/demo.gif
    version: v0.2.0
```

### Action inputs

| Input | Default | Meaning |
| --- | --- | --- |
| `script`      | *(required)* | Path to the `.tape` script. |
| `output`      | from script's `Output` directive | Output file path. Extension picks the renderer (`.gif` or `.svg`). |
| `version`     | `latest`     | evp release tag to install. |
| `font`        | embedded JetBrains Mono | Optional path to a TTF/OTF font for the GIF renderer. |
| `log-level`   | `info`       | One of `error`, `warn`, `info`, `debug`, `trace`. |
| `install-dir` | `/usr/local/bin` | Where to install the `evp` binary. Added to `$GITHUB_PATH`. |

### System requirements

The release tarball is a fully static `musl` + `static-pie` x86_64
binary, so it runs on every GitHub-hosted Linux runner (`ubuntu-22.04`,
`ubuntu-24.04`, etc.) without any glibc version concern.

Required at runtime: nothing — `ldd` reports `statically linked`. **No**
fontconfig, **no** display server, **no** ImageMagick, **no** ffmpeg,
**no** Zig. Fonts are embedded into the binary. See the top-level
[System requirements](#system-requirements) section for details.

### Self-hosted runners

The prebuilt tarball is x86_64-only. On non-amd64 hosts (arm64, macOS,
Windows) you have two options:

1. **Use the published Docker image** — see [Docker fallback](#docker-fallback)
   below.
2. **Build from source** — install Zig 0.15.x and Rust, then
   `cargo install --git https://github.com/HalFrgrd/evp evp`.

### Docker fallback

A multi-arch image is published on every push to `master` and on tag
pushes:

```yaml
- name: Render terminal demo
  run: |
    docker run --rm -v "$PWD:/work" \
      ghcr.io/halfrgrd/evp:latest \
      docs/demo.tape --output docs/demo.gif
```

The image is fully self-contained — `libghostty-vt.a` is statically
linked into the binary — so no extra `--volume` for fonts or libraries
is needed.

## Status

| Feature | State |
| --- | --- |
| `.tape` parsing (`Set` / `Type` / `Sleep` / `Wait` / `Hide` / `Show` / `Ctrl+X` / `Output` / `Env` / `Source` / `Require`) | working |
| PTY-backed shell, libghostty VT, diff-encoded `Recording` | working |
| GIF renderer | working |
| JSON serialisation of `Recording` | working |
| Animated SVG renderer | working (selectable text, ~10× smaller than GIF) |
| `Screenshot`, `Copy` / `Paste`, `Set Theme`, `Set Margin*`, `Set WindowBar`, `Set BorderRadius`, `Set LetterSpacing`, `Set CursorBlink`, `Set LoopOffset`, multiple `Output`, `.mp4` / `.webm` / `.txt` / `.ascii` / PNG-frames outputs | **not implemented — tape will fail loudly with a clear error** |

See [architecture.md](architecture.md) for the design rationale.
See the next section for the full VHS-vs-evp parity matrix.

## VHS feature parity

`evp` consumes the same `.tape` script format as
[charmbracelet/vhs](https://github.com/charmbracelet/vhs), but it is a
much smaller project — it only implements the subset of VHS that maps
cleanly onto an embedded libghostty + Rust renderer. Anything not in
that subset **fails loudly at parse or run time** rather than silently
no-op'ing, so a tape that produces a GIF on `evp` is guaranteed to have
exercised every directive it contains.

### Supported (matches VHS semantics)

| VHS directive | evp |
| --- | --- |
| `Output <path>` (single `.gif` / `.svg`) | ✅ |
| `Set Shell <command...>` | ✅ accepts a full command line (e.g. `bash`, `/bin/bash`, `bash --norc`, `bash --rcfile my.rc`) |
| `Set FontFamily <path-or-name>` (path to a TTF/OTF/TTC) | ⚠️ accepts a font *file path*; VHS resolves font *family names* via fontconfig. Pass `--font /path/to/font.ttf` on the CLI for the same effect. |
| `Set FontSize <n>` | ✅ |
| `Set Width <px>` / `Set Height <px>` | ✅ |
| `Set Padding <px>` | ✅ |
| `Set LineHeight <f>` | ✅ |
| `Set Framerate <n>` (also `FrameRate`, `FPS`) | ✅ |
| `Set PlaybackSpeed <f>` | ✅ |
| `Set TypingSpeed <duration>` | ✅ |
| `Set WaitTimeout <duration>` | ✅ |
| `Set WaitPattern /regex/` | ✅ |
| `Type[@<duration>] "text" ...` (single + double quotes + raw backticks) | ✅ |
| `Sleep <duration>` | ✅ |
| `Wait[+Screen|+Line][@<duration>] [/regex/]` | ✅ |
| `Hide` / `Show` | ✅ |
| `Backspace`, `Delete`, `Insert`, `Enter`, `Tab`, `Space`, `Escape` (with optional `@<duration> <count>`) | ✅ |
| `Up` / `Down` / `Left` / `Right` / `PageUp` / `PageDown` / `Home` / `End` | ✅ |
| `ScrollUp` / `ScrollDown` (modelled as keys; see "Differences" below) | ⚠️ |
| `Ctrl[+Alt][+Shift]+<char>` | ✅ |
| `Env <KEY> <VALUE>` | ✅ |
| `Require <program>` (checked against `$PATH`; missing programs abort the run) | ✅ |
| `Source <path>` (recursive, cycle-detected) | ✅ |
| Comments (`#` to end-of-line) | ✅ |
| evp extras: `Set Cols <n>` / `Set Rows <n>` (explicit cell-grid override) | ✅ (no VHS equivalent) |

### Not implemented — tape fails loudly

If a tape uses any of the following, evp aborts with an error pointing
back to this section, instead of silently dropping the directive on the
floor:

| VHS directive | evp behaviour |
| --- | --- |
| `Set Theme <name|json>` | parse-time error (default Snazzy palette is used; no theme registry) |
| `Set LetterSpacing <px>` | parse-time error |
| `Set CursorBlink <bool>` | parse-time error (cursor block is currently always rendered as VHS would render `CursorBlink false`) |
| `Set LoopOffset <pct>` | parse-time error |
| `Set Margin <px>` / `Set MarginFill <color>` | parse-time error |
| `Set WindowBar <style>` / `Set WindowBarSize <px>` | parse-time error |
| `Set BorderRadius <px>` | parse-time error |
| `Screenshot <path>` | run-time error |
| `Copy "..."` / `Paste` | parse-time error |
| Multiple `Output` directives in one tape | parse-time error (use a separate tape per output) |
| `Output out.mp4` / `.webm` / `.txt` / `.ascii` / PNG frames directory | parse-time error (only `.gif` and `.svg` are written) |

### CLI / tooling not implemented

VHS ships several subcommands that evp does not provide:

- `vhs new <file>` — tape scaffolder
- `vhs record` — interactive ttyd recorder that writes a `.tape`
- `vhs publish <file>` — uploads a GIF to `vhs.charm.sh`
- `vhs serve` — SSH server that renders tapes for remote clients
- `vhs themes` — list bundled themes
- `vhs validate <file>` — parse-only check
- `vhs manual` — built-in command reference

For evp the only entry point is `evp <script>`; see [CLI](#cli) above.

### Differences that aren't bugs

These behaviours match VHS's documented semantics but rely on a
different implementation, so the visual or runtime output may not be
byte-identical:

- **Renderer.** VHS records ttyd in a headless browser and re-encodes
  with ffmpeg/gifski. evp drives an embedded libghostty VT and rasterises
  cell-by-cell with `ab_glyph` + `gifski`. Antialiasing, glyph metrics,
  and dithering will differ slightly from a VHS recording of the same
  tape — the *content* is the same, the pixels may not be.
- **Font resolution.** VHS uses the system font stack via the browser /
  fontconfig. evp ships JetBrains Mono embedded into the binary and
  treats `Set FontFamily` as a *file path* (or accepts `--font
  path/to/font.ttf` on the CLI). Passing a bare family name like
  `"JetBrains Mono"` will fall back to the embedded faces with a warning.
- **Default palette.** VHS defaults to a built-in dark theme; evp
  applies the Snazzy palette via OSC 4/10/11/12 at startup. Because
  `Set Theme` is rejected, a tape that omits it gets the same colours
  on every run — at the cost of not being able to switch themes.
- **`ScrollUp` / `ScrollDown`.** VHS implements these as smooth
  multi-frame mouse-wheel scrolls. evp models them as discrete key
  presses fed to the PTY; programs that respond to mouse wheel events
  in mouse-tracking mode (e.g. `less`) won't react to them.
- **Single output.** VHS allows multiple `Output` directives in one
  tape and renders each. evp restricts a tape to one output — split
  into separate tapes for multi-format renders.
- **GitHub Action.** VHS has
  [`charmbracelet/vhs-action`](https://github.com/charmbracelet/vhs-action);
  evp ships its own composite action — see
  [Using `evp` in GitHub Actions](#using-evp-in-github-actions). Their
  inputs are not interchangeable.
- **Zero runtime dependencies.** VHS requires `ttyd` and `ffmpeg` on
  `$PATH`. evp's release binary is statically linked and needs neither.
