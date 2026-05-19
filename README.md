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
- embedded fonts
- TODO: mouse support 
- TODO: font is embedded in the SVG so that it renders the same everywhere
- TODO: text is selectable


## Acknowledgments

### VHS

evp is based on the vhs project.
They share little code but evp does try use the same `.tape` file format.

The color themes in [`assets/vhs-themes.json`](assets/vhs-themes.json) are taken from the [VHS](https://github.com/charmbracelet/vhs) project and are licensed under the MIT License. See [licenses/VHS-MIT.txt](licenses/VHS-MIT.txt) for the full license text.

Copyright (c) 2022-2023 Charmbracelet, Inc
