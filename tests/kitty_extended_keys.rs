mod common;

use common::{full_haystack, record};

#[test]
fn kitty_extended_keys_are_recorded_from_shell_program() {
    let key_debug_bin = common::get_helper_bin_path();
    let tape = format!(
        r#"
Output out.gif
Set Width 2200
Set Height 300
Set FontSize 20
Set Framerate 30
Set Shell {key_debug_bin}
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
Sleep 120ms
q
Sleep 300ms
"#
    );

    let rec = record(&tape);
    let haystack = full_haystack(&rec);

    assert!(
        haystack.contains("Press any key sequence"),
        "expected key debug app readiness"
    );
    assert!(
        haystack.contains("counter=3 key=KeyEvent { code: Enter, modifiers: KeyModifiers(CONTROL), kind: Press, state: KeyEventState(0x0) }"),
        "expected Ctrl+Enter event in output; haystack tail:\n{}",
        &haystack[haystack.len().saturating_sub(2_000)..]
    );
    assert!(
        haystack.contains("counter=4 key=KeyEvent { code: Char('c'), modifiers: KeyModifiers(SUPER), kind: Press, state: KeyEventState(0x0) }"),
        "expected Command+c event in output"
    );
    assert!(
        haystack.contains("counter=5 key=KeyEvent { code: Char('c'), modifiers: KeyModifiers(SUPER), kind: Release, state: KeyEventState(0x0) }"),
        "expected release events from kitty extended keyboard protocol"
    );
    assert!(
        !haystack.contains("code: Char('q')"),
        "expected plain q to exit without being printed"
    );
}
