# Mouse demo: demonstrates all script-level mouse operations in evp.
#
# Runs the evp helper tool in mouse capture mode.

Output mouse.gif
Output mouse.svg


Set Shell ./target/x86_64-unknown-linux-musl/release/evp_helper_tool mouse
Set Width 1200
Set Height 600
Set FontSize 20
Set Framerate 30
Set Padding 0
Set Margin 0

# Wait for alternate screen transition
Sleep 500ms

# Click at various places (turns cells Red)
Click 10 5
Sleep 200ms
Click 30 5
Sleep 200ms

# Right click (turns cells Green)
RightClick 20 10
Sleep 200ms

# Mouse scroll
MouseScroll 20 5 Up
Sleep 200ms
MouseScroll 20 6 Down
Sleep 200ms

# Move mouse around without clicking (turns cells Light Blue)
MouseMove 5 8 15 8
Sleep 200ms

# Drag mouse (turns cells Blue)
MouseDrag 15 8 25 8
Sleep 200ms

# Quit the helper tool by typing "q"
Type "q"
Sleep 500ms
