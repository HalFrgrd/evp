# Meta-demo: records evp being used from the command line.
# The resulting recording of this tape is what appears at the top of the README.

Output evp_demo.gif

Set Shell bash --norc
Set Width 1500
Set Height 600
Set FontSize 18
Set TypingSpeed 60ms
Set Framerate 30
Set WaitTimeout 60s

Hide
Sleep 500ms
# Clear any old demo files to ensure clean outputs
Type " rm -f demo.tape demo.gif && clear"
Enter
Sleep 300ms
Show

# 1. Run evp record
Type "evp record --theme Snazzy"
Sleep 500ms
Enter
Sleep 1.5s

# Inside evp record subshell
Type "echo 'Welcome to EVP interactive recording!'"
Sleep 500ms
Enter
Sleep 1s
Type "# While recording, your inputs are saved to demo.tape and this inner screen is saved to demo.gif"
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

Type "# You can fine tune the recording by editing demo.tape and re-running it with 'evp demo.tape'"
Sleep 500ms
Enter
Sleep 1.5s

# 3. Edit the tape file using vim
Type "vim -c 'set syntax=elixir' demo.tape"
Sleep 500ms
Enter
Sleep 2s

Type "/^Set FontSize"
Sleep 1s
Enter
Sleep 1s

Type "w"
Sleep 1s
Type "w"
Sleep 1s
Type "dw"
Sleep 1s

Type "i"
Sleep 500ms
Type " 24"
Sleep 500ms
Escape
Sleep 1s

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
