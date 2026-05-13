mod common;

use common::{full_haystack, record};

#[test]
fn kitty_extended_keys_are_recorded_from_shell_program() {
    let key_debug_bin = env!("CARGO_BIN_EXE_kitty_key_debug");
    let tape = format!(
        r#"
Output out.gif
Set Width 800
Set Height 300
Set FontSize 20
Set Framerate 30
Set Shell {key_debug_bin}
Env EVP_KITTY_KEY_EVENTS 6
Sleep 400ms
Press Command
Sleep 120ms
Release Command
Sleep 120ms
Ctrl+Enter
Sleep 120ms
Command+c
Sleep 120ms
Release Command+c
Sleep 120ms
Alt+z
Sleep 600ms
"#
    );

    let rec = record(&tape);
    let haystack = full_haystack(&rec);

    assert!(
        haystack.contains("ready"),
        "expected key debug app readiness"
    );
    assert!(
        haystack.contains("codepoint=13") && haystack.contains("mods=CTRL"),
        "expected Ctrl+Enter event in output; haystack tail:\n{}",
        &haystack[haystack.len().saturating_sub(2_000)..]
    );
    assert!(
        haystack.contains("codepoint=99") && haystack.contains("mods=SUPER"),
        "expected Command+c event in output"
    );
    assert!(
        haystack.contains("kind=Release") && haystack.contains("raw=\\x1b["),
        "expected release events from kitty extended keyboard protocol"
    );
}
