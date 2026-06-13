# Visual example demonstrating Set Padding.
#
# Padding defines the inner space (in pixels) between the terminal grid
# and the outer window frame.

Output padding.gif

Set Shell bash
Set Width 800
Set Height 400
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30

Set Padding 60
Set Margin 20
Set MarginFill "#6B50FF"

Sleep 500ms
Type "echo 'Welcome to evp!'"
Sleep 200ms
Enter
Sleep 500ms
Type "echo 'This terminal has Set Padding 60.'"
Sleep 200ms
Enter
Sleep 1.5s
