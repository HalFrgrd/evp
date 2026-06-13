# Visual example demonstrating Set Margin and Set MarginFill.
#
# Margin defines the outer space (in pixels) around the terminal window.
# MarginFill sets the background color of that margin area.

Output margin.gif

Set Shell bash
Set Width 800
Set Height 400
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30

Set Padding 10
Set Margin 60
Set MarginFill "#FF5733"

Sleep 500ms
Type "echo 'Welcome to evp!'"
Sleep 200ms
Enter
Sleep 500ms
Type "echo 'This terminal has Set Margin 60 with MarginFill #FF5733.'"
Sleep 200ms
Enter
Sleep 1.5s
