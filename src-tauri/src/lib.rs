//! Tauri composition root for Tan: wires adapters to use cases and exposes IPC commands.

pub mod commands;

use specta_typescript::Typescript;
use tauri_specta::{collect_commands, Builder};

/// Location of the generated TypeScript bindings, relative to this crate's manifest.
pub const BINDINGS_PATH: &str = "../ui/src/ipc/bindings.ts";

/// Builds the IPC registry: the single source of truth for commands exposed to the UI.
#[must_use]
pub fn ipc_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![commands::app_version])
}

/// Exports the TypeScript bindings for every registered IPC command to `path`.
///
/// # Errors
///
/// Returns an error if the bindings cannot be generated or written.
pub fn export_bindings(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    ipc_builder().export(Typescript::default(), path)?;
    Ok(())
}

/// Starts the application.
///
/// # Panics
///
/// Panics if the Tauri runtime fails to start.
pub fn run() {
    let builder = ipc_builder();

    tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Tan application");
}
