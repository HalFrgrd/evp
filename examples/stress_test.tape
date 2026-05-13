# Stress-test tape for the evp/VHS rendering pipeline.
#
# Drives `scripts/stress_test_program.py` (mounted/copied to /tmp/stress_test_program.py
# by the workflow) which redraws *every cell* of a 100×30 grid with a
# random ASCII codepoint + random fg/bg + random modifiers on every byte
# it receives. The recording captures at 50 fps and the script types at
# 50 chars/second (one keystroke per captured frame) for 5 s, which
# is the worst-case render workload.
#
# The same tape is consumed by both `evp` and `vhs`. evp accepts
# `Set Cols`/`Set Rows` directly; VHS infers the grid from
# `Set Width`/`Set Height` so we set both — evp uses Cols/Rows when
# present, and VHS uses Width/Height. The pixel sizes here match a
# 100×30 grid at FontSize 14 with line-height 1.0.

Output stress_test.gif

# VHS expects the shell setting to be the bare executable name.
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
Set Framerate 50

# Fast typing for the prelude so it doesn't eat into the stress_test window.
Set TypingSpeed 1ms
Hide
Type "exec python3 /tmp/stress_test_program.py"
Enter
Sleep 500ms
Show

# Stress-test run. 20 ms per char = 50 keystrokes/sec, matching the 50 fps
# capture rate so every captured frame sees a repaint. 250 chars × 20 ms
# = 5 s of typing, then a short trailing Sleep so the final state is
# captured. The stress_test program ignores the byte values and repaints
# the full 100×30 grid on every received byte.
Set TypingSpeed 20ms
Type "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKL"
Type "MNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmn"
Type "opqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
Sleep 200ms
