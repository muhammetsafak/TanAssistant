//! Global keyboard shortcuts (ADR-014).
//!
//! One shortcut exists: it toggles the microphone mute of the running recording, so a meeting
//! participant can mute from any application. It only does anything while a recording runs
//! (or is paused); otherwise the key press is ignored. The shortcut is registered from Rust
//! only: the capability of the main window grants no shortcut permission, so the UI cannot
//! register or inspect shortcuts.

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Error, GlobalShortcutExt, Shortcut, ShortcutState};

use crate::commands::recording;

/// The accelerator that toggles the microphone mute. `Ctrl+Shift+M` is deliberately not used:
/// it is Microsoft Teams' own mute shortcut.
pub const MUTE_MICROPHONE_SHORTCUT: &str = "CmdOrCtrl+Alt+M";

/// Registers the global shortcuts.
///
/// # Errors
///
/// Fails when the plugin cannot start or the OS refuses the shortcut (another application
/// already owns it, no display server...). The caller logs it: the application works without
/// the shortcut.
pub fn init(app: &AppHandle) -> Result<(), Error> {
    let shortcut: Shortcut = MUTE_MICROPHONE_SHORTCUT.parse().map_err(Error::from)?;
    app.plugin(tauri_plugin_global_shortcut::Builder::new().build())?;
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                toggle_microphone_mute(app);
            }
        })
}

/// Toggles the microphone mute, if a recording is running or paused.
fn toggle_microphone_mute(app: &AppHandle) {
    if !(recording::is_recording(app) || recording::is_paused(app)) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = recording::toggle_microphone_mute(&app).await {
            tracing::warn!(code = ?error.code, "{}", error.message);
        }
    });
}
