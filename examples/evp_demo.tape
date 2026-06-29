# Meta-demo: records evp being used from the command line.
# The resulting recording of this tape is what appears at the top of the README.

Output evp_demo.gif

Set Shell bash --norc
Set Width 1200
Set Height 600
Set FontSize 18
Set TypingSpeed 40ms
Set Framerate 30
Set WaitTimeout 120s

Sleep 500ms
# Clear any old demo files to ensure clean outputs
Type "rm -f demo.tape demo.gif"
Enter
Sleep 300ms

# 1. Run evp record
Type "evp record --rows 12 --cols 80"
Sleep 500ms
Enter
Sleep 1.5s

# Inside evp record subshell
Type "echo 'Welcome to EVP interactive recording!'"
Sleep 500ms
Enter
Sleep 1s
Type "exit"
Sleep 500ms
Enter
Sleep 2.5s

# 2. Show ls -l demo*
Type "ls -l demo*"
Sleep 500ms
Enter
Sleep 1.5s

# 3. Edit the tape file using vim
Type "vim demo.tape"
Sleep 500ms
Enter
Sleep 1.5s
Type "i"
Sleep 200ms
Type "# Edited using Vim!"
Sleep 500ms
Enter
Escape
Sleep 500ms
Type ":wq"
Sleep 500ms
Enter
Sleep 1.5s

# 4. Run evp demo.tape
Type "evp demo.tape"
Sleep 500ms
Enter
Wait+Screen /evp: evp finished/
Sleep 1.5s

# 5. Run ls -l demo* again
Type "ls -l demo*"
Sleep 500ms
Enter
Sleep 2s
