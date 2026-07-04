# Example demonstrating the custom window bar title and font features.
#
# There are two ways to control the window bar title in EVP:
#
# 1. Static Custom Title & Font (configured directly in this script):
#    Set WindowBarTitle "My Custom Title"
#    Set WindowBarFontFamily "JetBrains Mono"
#    Set WindowBarFontSize 15
#
# 2. Dynamic Title updates (using OSC escape sequences as shown below):
#    printf '\033]2;My Custom Title\007'

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
