#!/usr/bin/env python3
"""Torture-test program for the evp/VHS rendering pipeline.

For every byte received on stdin, the program redraws every cell of a
COLS×ROWS terminal grid with:

  * a fresh random printable ASCII codepoint (0x21..0x7E),
  * a random 24-bit foreground colour,
  * a random 24-bit background colour,
  * a random combination of bold / italic / underline / inverse modifiers.

Effectively zero per-cell memoisation, so every keystroke produces the
worst-case "every cell changed" diff for the encoder/renderer to chew
on. The program intentionally ignores the *value* of the input — only
the keystroke count matters — which lets the recording driver type at
any rate it likes.

Configurable via env vars:

    TORTURE_COLS    grid width  (default 100)
    TORTURE_ROWS    grid height (default 30)
    TORTURE_SEED    PRNG seed   (default 0xEEA — deterministic output)

Writes a final newline + cursor-show on EOF / SIGINT so the host shell
isn't left in a weird state.
"""

from __future__ import annotations

import os
import random
import sys


COLS = int(os.environ.get("TORTURE_COLS", "100"))
ROWS = int(os.environ.get("TORTURE_ROWS", "30"))
SEED = int(os.environ.get("TORTURE_SEED", "0xEEA"), 0)

# ASCII printable range minus space (so every cell visibly changes).
ASCII_MIN = 0x21
ASCII_MAX = 0x7E

# SGR modifier codes we randomly mix together.
MODIFIERS = (1, 3, 4, 7)  # bold, italic, underline, inverse


def render_frame(rng: random.Random, out) -> None:
    """Emit one full-screen redraw to `out` as a single buffered write."""
    parts: list[str] = []
    # Reset, hide cursor, home.
    parts.append("\x1b[0m\x1b[?25l\x1b[H")
    for row in range(1, ROWS + 1):
        parts.append(f"\x1b[{row};1H")
        for _col in range(COLS):
            ch = chr(rng.randint(ASCII_MIN, ASCII_MAX))
            fg_r = rng.randint(0, 255)
            fg_g = rng.randint(0, 255)
            fg_b = rng.randint(0, 255)
            bg_r = rng.randint(0, 255)
            bg_g = rng.randint(0, 255)
            bg_b = rng.randint(0, 255)
            # Pick 0..3 random modifiers.
            mod_count = rng.randint(0, len(MODIFIERS))
            mods = rng.sample(MODIFIERS, mod_count) if mod_count else ()
            sgr = ["0"] + [str(m) for m in mods]
            sgr.append(f"38;2;{fg_r};{fg_g};{fg_b}")
            sgr.append(f"48;2;{bg_r};{bg_g};{bg_b}")
            parts.append("\x1b[" + ";".join(sgr) + "m")
            parts.append(ch)
    parts.append("\x1b[0m")
    out.write("".join(parts))
    out.flush()


def main() -> int:
    rng = random.Random(SEED)
    # Initial paint so the terminal isn't blank before the first keypress.
    try:
        render_frame(rng, sys.stdout)
    except BrokenPipeError:
        return 0

    stdin_fd = sys.stdin.fileno()
    while True:
        try:
            data = os.read(stdin_fd, 1)
        except (KeyboardInterrupt, OSError):
            break
        if not data:
            break
        try:
            render_frame(rng, sys.stdout)
        except BrokenPipeError:
            break

    # Restore cursor on exit so the host shell isn't left with it hidden.
    try:
        sys.stdout.write("\x1b[0m\x1b[?25h\n")
        sys.stdout.flush()
    except BrokenPipeError:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
