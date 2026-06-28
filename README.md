# EVP

<div align="center">

[![CI](https://github.com/HalFrgrd/evp/actions/workflows/ci.yml/badge.svg)](https://github.com/HalFrgrd/evp/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/HalFrgrd/evp)](https://github.com/HalFrgrd/evp/blob/master/LICENSE)
[![Latest Release](https://img.shields.io/github/v/release/HalFrgrd/evp)](https://github.com/HalFrgrd/evp/releases)

**EVP is a Rust CLI tool to make beautiful terminal recordings.**

</div>

You write a `.tape` file and run `evp my_sript.tape` to produce smooth, high quality, portable GIF or SVG demos.
EVP is a rewrite of [VHS](https://github.com/charmbracelet/vhs) with some key improvements.

See it in action at [flyline](https://github.com/HalFrgrd/flyline) or below:

![Demo](https://github.com/HalFrgrd/evp/releases/download/assets/evp_demo.gif)


## VHS comparison
EVP is extends [VHS](https://github.com/charmbracelet/vhs) in these ways:
- Significantly faster. No more skipped frames when creating demos in GitHub Actions!
- Animated SVGs:
    - Super crisp demos
    - You can select and copy text from the SVG as it is playing!
    - Fonts are embedded into the SVG so the demos are portable (fonts are subsetted before embedding)
    - SVG screenshots
    - [Box-drawing chars](https://en.wikipedia.org/wiki/Box-drawing_characters) are rendered as SVG shapes. Don't worry, an invisible character is written to the SVG so selecting and copying text works as expected.
- Supports [Kitty extended keycodes](https://sw.kovidgoyal.net/kitty/keyboard-protocol/). e.g:
    - `Ctrl+Shift+Alt+Left`
    - `Command+Alt+PageUp`
- Specify full shell path with arguments. e.g.:
    - `Set Shell bash --init-file test.rc`
    - `Set Shell yazi` you can specify any program, not just a shell!
- EVP has no runtime dependencies
    - It is a statically linked [musl binary](https://www.musl-libc.org/)
    - A collection of useful fonts are embedded into EVP
- Mouse support: you can script mouse clicks, dragging, movement <img src="https://github.com/HalFrgrd/evp/releases/download/assets/mouse_small.gif" alt="mouse_small" width="500" />
- Set the terminal size by number of rows / cols or by number of pixels.
- Coming soon: resize support
- Coming soon: snapshot process metrics to help debug interactive use
- Coming soon: key event name overlay

## Technology
EVP runs a shell inside an embedded [libghostty](https://ghostty.org) terminal.
This allows us to support advanced terminal features in a headless environment such as: complex key combinations; key press and key release events; mouse input support; legacy escape codes.

The SVG animations are done using [SMIL](https://developer.mozilla.org/en-US/docs/Web/SVG/Guides/SVG_animation_with_SMIL).
Fonts are embedded into the SVG using [this method](https://stackoverflow.com/questions/79680618/how-to-embed-a-font-inside-a-svg-file).
A custom optimizer reduces the file size and complexity.
All the text on one terminal row is rendered inside of a `<text>` span which helps the browser copy the right text when you select and copy from the SVG.

GIF rendering is powered by [gifski](https://crates.io/crates/gifski).

## Install

#### Quick install: `install.sh`

> [!TIP]
> No `sudo` required!
```bash
curl -sSfL https://raw.githubusercontent.com/HalFrgrd/evp/master/install.sh | sh
```

#### Build locally
```bash
> docker bake extract-libghostty
> cargo build 
```

## Usage

#### CLI
```bash
# Start from a base script:
> evp print-ref-script > demo.tape
# Modify the script then run it!
> evp demo.tape
```
Or if you already have a VHS script:
```bash
> evp --mimic-vhs demo.tape
```

#### Record Interactive Sessions

You can record your live terminal sessions directly to a `.tape` file and a corresponding `.gif`!
EVP turns your terminal into raw mode and captures all keystrokes, mouse clicks, drags, movements, and scrolling:

```bash
# Record your session (saves to demo.tape and demo.gif by default)
> evp record

# Customize the outputs, shell, theme, and geometry:
> evp record --output my_demo.tape --output demo.svg --shell zsh --theme "Catppuccin Mocha" --cols 100 --rows 40
```

When you are done, simply exit the subshell (e.g. type `exit` or press `Ctrl+D`). EVP will instantly write the formatted `.tape` script to disk and compile the `.gif` in the background!

![EVP Record Demo](https://github.com/HalFrgrd/evp/releases/download/assets/evp_record_demo.gif)

#### GitHub Actions

You can run `evp` directly in your GitHub Actions workflows to automate rendering your terminal recordings. Since the action downloads a prebuilt static binary, no Rust toolchain or Docker setup is required:

```yaml
- name: Render tape script
  uses: HalFrgrd/evp@v0.14.0 # Replace with the desired release tag
  with:
    script: demo.tape
    output: demo.gif
```

#### Docker

EVP publishes a pre-configured Docker image containing the statically linked `evp` binary on a modern Ubuntu base. 

##### Interactive Demo Environment
To play around in the sandbox with a customized purple shell prompt and preloaded samples:
```bash
docker run -it ghcr.io/halfrgrd/evp:latest
```
Inside the container, you will find demo folders and a sample script ready to run:
```bash
cd ~/demos
evp demo.tape --output demo.gif
```

##### Rendering Local Scripts
To render your local `.tape` scripts without installing `evp` on your host system, mount your directory:
```bash
docker run --rm -v "$(pwd)":/work -w /work ghcr.io/halfrgrd/evp:latest evp my_script.tape
```

#### Custom docker



## Script Reference

EVP supports a superset of VHS `.tape` script language. Below is a complete template of all supported commands, settings, and events in the EVP `.tape` script language:

<!-- START_REF_SCRIPT -->
```elixir
# --- Output & Dependencies ---
Output demo.gif                     # Target output file (supports .gif, .svg, .svgz, .json, .stats)
Output demo.json                    # Multiple outputs are supported. You can use the `--ouput demo.svg` flag also.
Output demo.svg                     # Recommended
Output demo.svgz                    # gzip-compressed svg, less widely supported
Output demo.stats                   # Reports some stats about the rendering process
Require curl                        # Assert that curl is available on system PATH
Source common.tape                  # Inline another tape file

# --- Window & Font Settings ---
Set Shell bash                      # Configure shell execution path (e.g. bash, zsh, fish)
Set Shell bash --norc               # EVP supports any command as the "shell"
Set Shell yazi                      # You can boot right into your TUI

Set Theme "Catppuccin Mocha"        # Apply a predefined colour theme (run `evp themes` to discover themes)
Set Font "JetBrains Mono"           # Monospace font family name
Set FontSize 20                     # Font size in pixels
Set LineHeight 1.2                  # Line spacing multiplier
Set LetterSpacing 0                 # Letter spacing adjustment
Set Width 800                       # Terminal window width in pixels
Set Height 400                      # Terminal window height in pixels
Set Cols 100                        # Set terminal width in rows (Set Width has precedence)
Set Rows 30                         # Set terminal height in rows (Set Height has precedence)
Set Padding 10                      # Inner padding between terminal grid and window frame
Set Margin 20                       # Outer margin around window frame
Set MarginFill "#6B50FF"            # Background colour of outer margin area
Set Framerate 30                    # Recording framerate in FPS
Set TypingSpeed 50ms                # Default speed/interval for typed text

# --- Child Process Environment ---
Env PS1 "$ "                        # Set environment variables for the shell process

# --- Playback & Timeline Controls ---
Sleep 1s                            # Pause playback for a duration (e.g. 500ms, 1.5s, 2m)
Hide                                # Hide subsequent commands from recording output
Show                                # Resume recording commands to output files
Screenshot frame.png                # Capture the current terminal window frame as a PNG, SVG, or JSON
Screenshot frame.svg
Screenshot frame.json

# --- Clipboard Controls ---
Copy "text to clipboard"            # Copy a string into the system clipboard
Paste                               # Paste current clipboard contents into terminal

# --- Keyboard & Input Events ---
Type "echo 'hello'"                 # Type text at the current default TypingSpeed
Type@10ms "fast typing"             # Type text at an overridden speed of 10ms
Enter                               # Press key alias (equivalent to Key "Enter" / Key "Return")
Backspace 5                         # Repeat a key alias N times (press Backspace 5 times)
Ctrl+C                              # Press key combination modifier
Ctrl+Shift+Alt+Left                 # Even complex key codes are possible.
Press Down                          # Explicitly hold a key down
Release Down                        # Explicitly release a held key

# --- Mouse Controls ---
Click 10 20                         # Left click at column 10, row 20
RightClick 10 20                    # Right click at column 10, row 20
DoubleClick 10 20                   # Double click at column 10, row 20
MouseMove@100ms 0 0 10 10           # Move cursor from (0,0) to (10,10) using default sigmoid (EaseInOutCubic) easing
MouseMove@100ms@EaseInOutElastic 0 0 10 10 # Move cursor with EaseInOutElastic easing curve override
MouseDrag@EaseInOutQuad 0 0 10 10   # Left click and drag with EaseInOutQuad easing curve
MouseScroll 10 20 Up                # Scroll mouse wheel Up or Down at column 10, row 20

# --- Wait / Synchronization ---
Wait "Ready"                        # Wait for output matching text pattern
Wait /regex/                        # Wait for output matching regular expression
Wait+Screen /regex/                 # Wait scanning full screen instead of only the last line
Wait+Line /regex/                   # Explicitly wait scanning last line only (default)
Wait@1s /regex/                     # Wait with an overridden timeout of 1s (default is 15s)
Wait+Line@1s /regex/                # Wait scanning last line with 1s timeout override
Wait+Screen@1s /regex/              # Wait scanning full screen with 1s timeout override
```
<!-- END_REF_SCRIPT -->

## Examples

### Dynamic Window Title Bar

You can enable a window bar styled with macOS-like window controls and dynamically update the title displayed at the center of the bar.
Your program will need to use [OSC 2 or OSC 0](https://man7.org/linux/man-pages/man4/console_codes.4.html) to set the title.
For instance:

```elixir
# Enable window bar styling
Set WindowBar Rings

# Set a dynamic terminal title via ANSI OSC sequence
Type "printf '\033]2;My Dynamic Terminal Title\007'"
Enter
```

![Dynamic Window Title Bar Demo](https://github.com/HalFrgrd/evp/releases/download/assets/window_title.gif)

## Serving SVGs / SVGZs

Unfortunately, GitHub doesn't support embeddable SVGs in such a way that you can select text from the SVG.


## Acknowledgments

### VHS

EVP is based on the [VHS](https://github.com/charmbracelet/vhs) project.
They share little code but EVP does try use the same `.tape` file format.

The colour themes in [`assets/vhs-themes.json`](assets/vhs-themes.json) are taken from the [VHS](https://github.com/charmbracelet/vhs) project and are licensed under the MIT License. See [licenses/VHS-MIT.txt](licenses/VHS-MIT.txt) for the full license.

### Font providers

Some fonts are embedded inside the EVP binary. See licenses in [license](licenses/) for the full license text.
