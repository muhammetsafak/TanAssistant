//! The global shortcut is registered at startup and a failure there is only logged, so a
//! typo in the accelerator would silently remove the feature. This checks that it parses, and
//! that it is not Teams' own mute shortcut (`Ctrl+Shift+M`).

use tan_app_lib::shortcuts::MUTE_MICROPHONE_SHORTCUT;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

#[test]
fn the_mute_shortcut_parses_to_primary_modifier_alt_and_m() {
    let shortcut: Shortcut = MUTE_MICROPHONE_SHORTCUT
        .parse()
        .expect("the accelerator must parse");

    // `CmdOrCtrl` is Command on macOS and Control elsewhere.
    let primary = if cfg!(target_os = "macos") {
        Modifiers::SUPER
    } else {
        Modifiers::CONTROL
    };
    assert_eq!(
        shortcut,
        Shortcut::new(Some(primary | Modifiers::ALT), Code::KeyM)
    );
    assert_ne!(
        shortcut,
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyM),
        "Ctrl+Shift+M belongs to Microsoft Teams"
    );
}
