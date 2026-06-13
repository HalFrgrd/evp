# Mouse Small: simple mouse cursor demo.
#
# Demonstrates: typing text, moving the mouse cursor, and performing a click.

Output mouse_small.gif
Output mouse_small.svg

Set Shell bash
Set Width 800
Set Rows 4
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30
Set Padding 10
Set Margin 10


Hide
Sleep 100ms
Type@5ms "EVP will render a mouse cursor!"
Sleep 100ms
Show

MouseMove@150ms 10 1 25 1
Sleep 200ms
Click 25 1
Sleep 1s
