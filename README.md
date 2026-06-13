# evp

**evp** — a small Rust CLI
that ingests [VHS](https://github.com/charmbracelet/vhs)-format scripts
and produces GIF, SVG, or JSON outputs using
[libghostty](https://ghostty.org) as the underlying terminal emulator.

`evp` runs a real shell inside an embedded Ghostty VT, schedules typed
input from your `.tape` script onto an absolute timeline, snapshots the
terminal at the configured framerate, then streams frames to one or more
renderer threads.

`evp` is very similar to [VHS](https://github.com/charmbracelet/vhs) except:
- significantly faster. The gif is ready as soon as the recording finishes.
- all key codes possible
- specify full shell path with arguments
- SVG output (as well as gif)
- no runtime dependencies (everything is statically linked)
- embedded, character-subsetted fonts (so SVG renders the same everywhere)
- selectable SVG text
- TODO: mouse support
- TODO: resize support
- TODO: process metrics
- TODO: show the key inputs on screen overlay
- render from a json recording
- svg box drawing
- svg copiable text

## Output Formats

`evp` infers the output renderer from the file extension of your `Output` directives or `--output` CLI argument. The following formats are supported:
- **`.gif`**: Animated GIF.
- **`.svg`**: Animated SVG with embedded, character-subsetted fonts.
- **`.svgz`**: Compressed SVG. If the output path ends in `.svgz`, `evp` automatically Gzip-compresses the generated SVG.
- **`.json`**: The raw terminal frame recording in JSON format.

## Acknowledgments

### VHS

evp is based on the vhs project.
They share little code but evp does try use the same `.tape` file format.

The color themes in [`assets/vhs-themes.json`](assets/vhs-themes.json) are taken from the [VHS](https://github.com/charmbracelet/vhs) project and are licensed under the MIT License. See [licenses/VHS-MIT.txt](licenses/VHS-MIT.txt) for the full license text.

Copyright (c) 2022-2023 Charmbracelet, Inc
