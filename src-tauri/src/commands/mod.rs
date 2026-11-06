//! IPC commands. These are thin adapters: no business logic lives here.

pub mod ai;
pub mod credentials;
pub mod recording;
pub mod settings;
pub mod transcription;

/// Returns the application version (from the crate manifest).
#[tauri::command]
#[specta::specta]
#[must_use]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
