# Built-in smoke test — runs via `evp run-sample-script`.
#
# Designed to work on any POSIX system: only `/bin/sh` builtins and
# `printf` are used. Output goes to ./evp-test.gif and ./evp-test.svg
# in the cwd so the user can immediately open them.

Output evp-test.gif
Output evp-test.svg

Set Shell bash --norc
Set Width 900
Set Height 360
Set FontSize 20
Set TypingSpeed 60ms
Set Framerate 30

Sleep 400ms
Type "echo 'Hello from evp '$(uname -srm)"
Enter
Sleep 600ms
Type "for c in 31 32 33 34 35 36; do printf '\033[1;%sm  evp  \033[0m' $c; done; echo"
Enter
Sleep 800ms
Type "printf '\033[1;38;5;213m  it works!  \033[0m\n'"
Enter
Sleep 800ms

# Demonstrate Copy & Paste
Copy "echo 'Pasted from clipboard!'"
Paste
Enter
Sleep 1000ms

# Demonstrate Screenshot
Screenshot evp-screenshot.png
Sleep 1000ms
