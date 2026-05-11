# Render an ANSI colour table — a good visual stress-test for the cell
# diff recorder and the GIF / SVG renderers.
#
# Uses printf to lay down 8 bright + 8 normal colours via SGR sequences.

Output colors.gif

Set Shell bash --norc
Set Width 900
Set Height 400
Set FontSize 18
Set TypingSpeed 30ms
Set Framerate 30

Sleep 500ms
Type "for c in 30 31 32 33 34 35 36 37; do printf '\\033[1;%sm  %s  \\033[0m' $c $c; done; echo"
Enter
Sleep 500ms
Type "for c in 90 91 92 93 94 95 96 97; do printf '\\033[1;%sm  %s  \\033[0m' $c $c; done; echo"
Enter
Sleep 1500ms
