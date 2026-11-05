//! IPC commands. These are thin adapters: no business logic lives here.

pub mod credentials;
pub mod settings;

/// Returns the application version (from the crate manifest).
#[tauri::command]
#[specta::specta]
#[must_use]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
