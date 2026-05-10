# Torture-test tape for the evp/VHS rendering pipeline.
#
# Drives `scripts/torture_program.py` (mounted/copied to /tmp/torture_program.py
# by the workflow) which redraws *every cell* of a 100×30 grid with a
# random ASCII codepoint + random fg/bg + random modifiers on every byte
# it receives. The test types at 100 chars/min (one keypress every 600 ms)
# for 10 seconds at 60 fps, which is a worst-case render workload.
#
# The same tape is consumed by both `evp` and `vhs`. evp accepts
# `Set Cols`/`Set Rows` directly; VHS infers the grid from
# `Set Width`/`Set Height` so we set both — evp uses Cols/Rows when
# present, and VHS uses Width/Height. The pixel sizes here match a
# 100×30 grid at FontSize 14 with line-height 1.0.

Output torture.gif

Set Shell /bin/sh
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

# Now the actual torture run: 100 chars/min = one keypress every 600 ms.
# 17 keystrokes × 600 ms ≈ 9.6 s; trailing Sleep brings the recording
# window to ~10 s. The torture program ignores the byte values and
# repaints the full 100×30 grid on every received byte.
Set TypingSpeed 600ms
Type "abcdefghijklmnopq"
Sleep 1s
