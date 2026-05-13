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
Release Command
Ctrl+Enter
Command+c
Release Command+c
Alt+z
Sleep 600ms
"#
    );

    let rec = record(&tape);
    let haystack = full_haystack(&rec);

    assert!(haystack.contains("ready"), "expected key debug app readiness");
    assert!(
        haystack.contains("code=Enter") && haystack.contains("mods=KeyModifiers(CONTROL)"),
        "expected Ctrl+Enter event in output; haystack tail:\n{}",
        &haystack[haystack.len().saturating_sub(2_000)..]
    );
    assert!(
        haystack.contains("code=Char('c')") && haystack.contains("mods=KeyModifiers(SUPER)"),
        "expected Command+c event in output"
    );
    assert!(
        haystack.contains("kind=Release"),
        "expected release events from kitty extended keyboard protocol"
    );
}
