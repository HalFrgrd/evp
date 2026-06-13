# Mouse Small: simple mouse cursor demo.
#
# Demonstrates: typing text, moving the mouse cursor, and performing a click.

Output mouse_small.gif
Output mouse_small.svg

Set Shell bash
Set Width 800
Set Height 300
Set FontSize 20
Set TypingSpeed 50ms
Set Framerate 30
Set Padding 0
Set Margin 0

Sleep 500ms
Type "EVP will render a mouse cursor!"
Sleep 500ms

# Move mouse slowly from (10, 5) to (15, 5)
MouseMove@150ms 10 5 15 5
Sleep 200ms
Click 15 5
Sleep 1s
