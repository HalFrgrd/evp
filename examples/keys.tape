# Demonstrates key combinations and the `Hide` / `Show` directives.
#
# `Hide` suppresses event execution from being shown in the output (it is
# *not* a capture toggle – frames are always captured); typing then
# pasting via Ctrl+U / Ctrl+Y simulates a quick edit.

Output keys.gif

Set Shell bash --norc
Set Width 900
Set Height 400
Set FontSize 18
Set TypingSpeed 40ms
Set Framerate 30

Sleep 500ms

# Type something then nuke it with Ctrl+U (kill-line in many shells).
Type "echo this will be deleted"
Sleep 400ms
Ctrl+U
Sleep 300ms

# Now type the real command.
Type "printf 'Modifiers work: %s\\n' OK"
Enter
Sleep 1s
