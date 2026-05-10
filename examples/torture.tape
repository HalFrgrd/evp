# Torture-test tape for the evp/VHS rendering pipeline.
#
# Drives `scripts/torture_program.py` (mounted/copied to /tmp/torture_program.py
# by the workflow) which redraws *every cell* of a 100×30 grid with a
# random ASCII codepoint + random fg/bg + random modifiers on every byte
# it receives. The recording captures at 60 fps and the script types at
# ~125 chars/second (≥2 keystrokes per captured frame) for ~10 s, which
# is the worst-case render workload.
#
# The same tape is consumed by both `evp` and `vhs`. evp accepts
# `Set Cols`/`Set Rows` directly; VHS infers the grid from
# `Set Width`/`Set Height` so we set both — evp uses Cols/Rows when
# present, and VHS uses Width/Height. The pixel sizes here match a
# 100×30 grid at FontSize 14 with line-height 1.0.

Output torture.gif

Set Shell bash
Set FontSize 14
# Pixel geometry chosen so `evp::runner::derive_options` rounds to a
# clean 100×30 grid: cell_w = round(14*0.6) = 8 → 100*8 = 800 wide;
# cell_h = round(14*1.0) = 14 → 30*14 = 420 tall.
# VHS uses the same width/height directly when sizing its terminal,
# so both renderers see the same target dimensions.
Set Width 800
Set Height 420
Set Padding 0
Set Framerate 60

# Fast typing for the prelude so it doesn't eat into the torture window.
Set TypingSpeed 1ms
Hide
Type "exec python3 /tmp/torture_program.py"
Enter
Sleep 500ms
Show

# Torture run. 8 ms per char ≈ 125 keystrokes/sec — at 60 fps that's
# slightly over two updates per captured frame (the encoder/renderer
# never gets a quiet moment between repaints). 1300 chars × 8 ms
# ≈ 10.4 s of typing, then a short trailing Sleep so the final state
# is captured. The torture program ignores the byte values and
# repaints the full 100×30 grid on every received byte.
#
# 8 ms (vs. exactly 1000/120 = 8.333 ms) keeps the duration parseable
# by every VHS version: VHS only accepts integer-millisecond
# durations, and 8 ms guarantees ≥2 updates per 16.67 ms frame.
Set TypingSpeed 8ms
Type "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKL"
Type "MNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmn"
Type "opqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
Type "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzAB"
Type "CDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd"
Type "efghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP"
Type "QRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqr"
Type "stuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123"
Type "456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEF"
Type "GHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh"
Type "ijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRST"
Type "UVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuv"
Type "wxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567"
Sleep 500ms
