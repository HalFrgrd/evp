# Embedded font demo for evp's default JetBrains Mono faces.
#
# This tape intentionally types glyphs that are likely outside JetBrains
# Mono's coverage so you can quickly evaluate fallback behavior.

Output embedded-font-demo.gif

Set Shell bash --norc
Set Width 1100
Set Height 420
Set FontSize 24
Set TypingSpeed 25ms
Set Framerate 30

Env PROMPT_COMMAND ""
Env PS1 "> "

Sleep 400ms
Type "printf '%s\n' 'Embedded symbols: 🯁🯂🯃 🬛 🮐 🮑 🮔 🮖 🮘 ⣾ ⣷ ⣯ ⣟ ⡿ ⢿ ⣻ ⣽'"
Enter
Sleep 700ms

Type "printf '%s\n%s\n%s\n' '╭─────────────╮' '│   hello!    │' '╰─────────────╯'"
Enter
Sleep 700ms

Type "printf '%s\n' 'Powerline symbols:        '"
Enter
Sleep 700ms

Type "printf '%s\n' 'Half-width katakana: ｱ ｲ ｳ ｴ ｵ ｶ ｷ ｸ ｹ ｺ ｻ ｼ ｽ ｾ ｿ ﾀ ﾁ ﾂ ﾃ ﾄ ﾅ ﾆ ﾇ ﾈ ﾉ ﾊ ﾋ ﾌ ﾍ ﾎ ﾏ ﾐ ﾑ ﾒ ﾓ ﾔ ﾕ ﾖ ﾗ ﾘ ﾙ ﾚ ﾛ ﾜ ｦ'"
Enter
Sleep 700ms

Type "printf '%s\n' 'Emojis: 🚀'"
Enter

Sleep 1500ms
