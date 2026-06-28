# Demo of the interactive record subcommand (evp record).
# Types the command to start recording, runs some commands in the subshell,
# and exits to show the compilation.

Output evp_record_demo.gif

Set Shell bash --norc
Set Width 1500
Set Height 600
Set FontSize 18
Set TypingSpeed 60ms
Set Framerate 30
Set WaitTimeout 60s

Sleep 500ms
Type "evp record --theme 'Snazzy'"
Sleep 1s
Enter
Sleep 2s
Type `echo "Welcome to EVP interactive recording!"`
Enter
Sleep 1s
Type "ls -la"
Enter
Sleep 1s
Type "exit"
Enter
# Wait for evp record to compile and exit back to standard shell
Wait+Screen /successfully/
Sleep 2s
