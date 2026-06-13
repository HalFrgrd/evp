# EVP

EVP is a Rust CLI tool to make beautiful terminal recordings.
You write a `.tape` file and run `evp my_sript.tape` to produce smooth, high quality `gif` or `svg` demos.

EVP is a rewrite of [VHS](https://github.com/charmbracelet/vhs) with some key improvements.

EVP runs a real shell inside an embedded [libghostty](https://ghostty.org) terminal.

## VHS comparison
EVP is extends [VHS](https://github.com/charmbracelet/vhs) in these ways:
- Significantly faster. No more skipped frames when creating demos in GHA!
- Supports [kitty extended keycodes](https://sw.kovidgoyal.net/kitty/keyboard-protocol/). e.g:
    - Ctrl+Shift+Alt+Left
    - Ctrl+Alt+Shift+P
- Supports animated SVGs:
    - Super crisp demos
    - You can select and copy text from the SVG as it is playing!
    - Fonts are embedded into the SVG so the demos are portable (fonts are subsetted before embedding)
    - SVG screenshots
    - [Box-drawing chars](https://en.wikipedia.org/wiki/Box-drawing_characters) are rendered as SVG shapes. Don't worry, an invisible character is still written to the SVG so selecting and copying text works as expected.
- Specify full shell path with arguments. e.g.:
    - `Set Shell bash --init-file test.rc`
    - `Set Shell yazi` you can specify and program, not just a shell!
- EVP has no runtime dependencies
    - It is a statically linked [musl binary](https://www.musl-libc.org/)
    - A collection of useful fonts are embedded into EVP
- Mouse support
    - 
- Coming soon: resize support
- Coming soon: snapshot process metrics to help debug interactive use
- Coming soon: key event overlay

## Output Formats

EVP infers the output renderer from the file extension of your `Output` directives or `--output` CLI argument. 
You can specify multiple outputs.
The following formats are supported:
- **`.gif`**: Animated GIF.
- **`.svg`**: Animated SVG with embedded, character-subsetted fonts.
- **`.svgz`**: Compressed SVG. If the output path ends in `.svgz`, EVP automatically Gzip-compresses the generated SVG.
- **`.json`**: The raw terminal frame recording in JSON format.
- **`.stats`**: JSON stats about the recording and rendering process.

## Script Reference

EVP supports a superset of VHS `.tape` script language.

Below is a complete template of all supported commands, settings, and events in the EVP `.tape` script language:

```elixir
# --- Output & Dependencies ---
Output demo.gif                     # Target output file (supports .gif, .svg, .svgz, .json, .stats)
Output demo.json
Output demo.svg
Require curl                        # Assert that curl is available on system PATH
Source common.tape                  # Inline another tape file's commands and settings

# --- Window & Font Settings ---
Set Shell bash                      # Configure shell execution path (e.g. bash, zsh, fish)
Set Shell bash --norc               # EVP supports any command as the "shell"
Set Shell yazi                      # You can boot right into your TUI

Set Theme "Catppuccin Mocha"        # Apply a predefined color theme (e.g. Catppuccin Mocha, Dracula)
Set Font "JetBrains Mono"           # Monospace font family name
Set FontSize 20                     # Font size in pixels
Set LineHeight 1.2                  # Line spacing multiplier
Set LetterSpacing 0                 # Letter spacing adjustment
Set Width 800                       # Terminal window width in pixels
Set Height 400                      # Terminal window height in pixels
Set Padding 10                      # Inner padding between terminal grid and window frame
Set Margin 20                       # Outer margin around window frame
Set MarginFill "#6B50FF"            # Background color of outer margin area
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
Ctrl+Shift+Alt+Left                 # 
Press Down                          # Explicitly hold a key down
Release Down                        # Explicitly release a held key

# --- Mouse Controls ---
Click 10 20                         # Left click at column 10, row 20
RightClick 10 20                    # Right click at column 10, row 20
DoubleClick 10 20                   # Double click at column 10, row 20
MouseMove 0 0 10 10                 # Move cursor from (0,0) to (10,10)
MouseDrag 0 0 10 10                 # Left click and drag from (0,0) to (10,10)
MouseScroll 10 20 Up                # Scroll mouse wheel Up or Down at column 10, row 20

# --- Wait / Synchronization ---
Wait "Ready"                        # Wait for output matching text pattern
Wait /regex/                        # Wait for output matching regular expression
```

## Acknowledgments

### VHS

EVP is based on the vhs project.
They share little code but EVP does try use the same `.tape` file format.

The color themes in [`assets/vhs-themes.json`](assets/vhs-themes.json) are taken from the [VHS](https://github.com/charmbracelet/vhs) project and are licensed under the MIT License. See [licenses/VHS-MIT.txt](licenses/VHS-MIT.txt) for the full license text.

### Font providers

Some fonts are embedded inside the EVP binary. See licenses in [license](licenses/) for the full license text.
