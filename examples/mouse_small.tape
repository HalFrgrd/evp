# Mouse Small: simple mouse cursor demo.
#
# Demonstrates: typing text, moving the mouse cursor, and performing a click.

Output mouse_small.gif
Output mouse_small.svg

Set Shell bash
Set Width 500
Set Height 120
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30
Set Margin 0
Set Padding 20

# Hide the typing animation
Hide
Sleep 100ms
Type@5ms "EVP will render a mouse cursor!"
Show

MouseMove@150ms 10 1 25 1
Sleep 500ms
Click 15 1
Sleep 1s
