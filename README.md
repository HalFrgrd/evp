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
# → ./docker/build/evp  (≈6 MiB, static-pie, stripped)
```

The `extract-binary` target writes the binary directly to the host
filesystem via buildx's local output writer — no `docker create` /
`docker cp` plumbing. Other targets in the same bake file:

```bash
docker buildx bake test       # workspace cargo test (CI parity)
docker buildx bake runtime    # builds evp:local container image
docker buildx bake builder    # intermediate builder image
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
| `.tape` parsing (Set / Type / Sleep / Wait / Hide / Show / Ctrl+X / Output / Env) | working |
| PTY-backed shell, libghostty VT, diff-encoded `Recording` | working |
| GIF renderer | working |
| JSON serialisation of `Recording` | working |
| Animated SVG renderer | working (selectable text, ~10× smaller than GIF) |
| `Screenshot` PNG export | TODO |
| Theme support, `Source`, `Copy` / `Paste` | parsed, no-op |

See [architecture.md](architecture.md) for the design rationale.
