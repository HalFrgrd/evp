# A tiny "progress bar" animation. Exercises the renderer's idle-frame
# skipping (long pauses with no change) versus high-churn frames (rapid
# in-place rewrites of the same line).

Output progress.gif

Set Shell /bin/sh
Set Width 900
Set Height 250
Set FontSize 20
Set TypingSpeed 30ms
Set Framerate 30

Sleep 500ms
Type "for i in 1 2 3 4 5 6 7 8 9 10; do printf '\\rprogress: [%-10s] %s%%' \"$(printf '#%.0s' $(seq 1 $i))\" $((i*10)); sleep 0.15; done; echo"
Enter
Sleep 2s
