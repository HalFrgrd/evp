# Meta-demo: records evp being used from the command line.
# The shell session below types `evp examples/my_program_demo.tape` and waits
# for evp to finish rendering the GIF.  The resulting recording of *this* tape
# is what appears at the top of the README.

Output evp_demo.gif

Set Shell bash --norc
Set Width 1400
Set Height 600
Set FontSize 18
Set TypingSpeed 40ms
Set Framerate 30
Set WaitTimeout 120s

Sleep 500ms
Type "evp examples/my_program_demo.tape --output my_program_demo.stats"
Sleep 500ms
Enter
Wait+Screen "evp: evp finished"
Sleep 1s
