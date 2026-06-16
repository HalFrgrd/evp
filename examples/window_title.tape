# Example demonstrating the custom window bar title feature.
#
# Runs a script that enables the window bar and sets a custom title using
# OSC escape sequences (e.g., printf '\033]2;My Custom Title\007').

Output window_title.gif
Output window_title.svg

Set Shell bash --norc
Set Width 900
Set Height 360
Set FontSize 20
Set TypingSpeed 60ms
Set Framerate 30
Set WindowBar Rings
Set Padding 20

Sleep 400ms
Type "printf '\033]2;My Dynamic Terminal Title\007'"
Enter
Sleep 600ms
Type "echo 'Notice the window title bar has changed above!'"
Enter
Sleep 1s
Type "printf '\033]2;Finished Demo\007'"
Enter
Sleep 1200ms
