# Showcase a few common shell built-ins typed at human speed.
#
# Demonstrates: multiple commands, longer typing pauses, Wait directive on
# a regex so we don't hard-code a Sleep duration.

Output shell-tour.gif

Set Shell bash --norc
Set Width 1000
Set Height 500
Set FontSize 18
Set TypingSpeed 40ms
Set Framerate 30
Set WaitTimeout 5s
Set WaitPattern "\$"

Sleep 500ms
Type "echo 'Welcome to evp'"
Enter
Wait

Type "uname -srm"
Enter
Wait

Type "for i in 1 2 3; do echo line $i; done"
Enter
Wait

Type "date -u +%Y-%m-%dT%H:%M:%SZ"
Enter
Sleep 1s
