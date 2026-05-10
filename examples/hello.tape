# Hello world: the simplest possible evp recording.
#
# Demonstrates: typing, a single command, and a short pause to capture the
# shell's output.

Output hello.gif

Set Shell bash --norc
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30

Sleep 500ms
Type "echo Hello, evp!"
Sleep 200ms
Enter
Sleep 1s
