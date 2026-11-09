//! Tauri composition root for Tan: wires adapters to use cases and exposes IPC commands.

pub mod commands;
pub mod error;
pub mod logging;
pub mod state;
pub mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use specta_typescript::Typescript;
use tan_domain::UiLanguage;
use tauri::{Manager, RunEvent, WindowEvent};
use tauri_specta::{collect_commands, collect_events, Builder};

use crate::commands::recording::TauriRecordingEvents;
use crate::state::{resolve_layout, AppState};

/// Location of the generated TypeScript bindings, relative to this crate's manifest.
pub const BINDINGS_PATH: &str = "../ui/src/ipc/bindings.ts";

/// Builds the IPC registry: the single source of truth for commands exposed to the UI.
#[must_use]
pub fn ipc_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::app_version,
            commands::ai::list_provider_profiles,
            commands::ai::upsert_provider_profile,
            commands::ai::delete_provider_profile,
            commands::ai::test_provider,
            commands::ai::list_provider_kinds,
            commands::ai::get_task_assignments,
            commands::ai::set_task_assignment,
            commands::ai::clear_task_assignment,
            commands::credentials::list_variables,
            commands::credentials::upsert_variable,
            commands::credentials::delete_variable,
            commands::credentials::list_secrets,
            commands::credentials::create_secret,
            commands::credentials::update_secret,
            commands::credentials::delete_secret,
            commands::credentials::reveal_secret,
            commands::credentials::secret_store_status,
            commands::credentials::unlock_secret_store,
            commands::credentials::set_master_password,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::recording::start_recording,
            commands::recording::stop_recording,
            commands::recording::pause_recording,
            commands::recording::resume_recording,
            commands::recording::recording_status,
            commands::recording::recording_capabilities,
            commands::recording::list_audio_devices,
            commands::recording::list_displays,
            commands::recording::display_thumbnail,
            commands::transcription::list_whisper_models,
            commands::transcription::download_whisper_model,
            commands::transcription::select_whisper_model,
            commands::transcription::delete_whisper_model,
            commands::transcription::transcribe_session,
            commands::transcription::estimate_transcription,
            commands::processing::process_session,
            commands::processing::retry_processing,
            commands::processing::cancel_processing,
            commands::processing::processing_status,
            commands::sessions::list_sessions,
            commands::sessions::search_sessions,
            commands::sessions::get_session_detail,
            commands::sessions::toggle_action_item,
            commands::sessions::export_session_markdown,
            commands::sessions::export_session_audio,
            commands::sessions::get_storage_usage,
            commands::sessions::delete_session,
        ])
        .events(collect_events![
            commands::recording::RecordingProgressEvent,
            commands::recording::RecordingStateEvent,
            commands::recording::RecordingWarningEvent,
            commands::transcription::WhisperModelDownloadEvent,
            commands::transcription::TranscriptionProgressEvent,
            commands::processing::ProcessingProgressEvent,
        ])
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

/// How often the audio-retention policy runs after the run at startup.
const RETENTION_INTERVAL: Duration = Duration::from_hours(24);

/// Applies the audio-retention policy now and then once a day, on a background thread (it
/// deletes files, which must not block the UI). The thread ends with the process.
fn spawn_retention(retention: std::sync::Arc<state::Retention>) {
    let spawned = std::thread::Builder::new()
        .name("audio-retention".to_owned())
        .spawn(move || loop {
            match tauri::async_runtime::block_on(retention.execute()) {
                Ok(report) if report.files_deleted > 0 || report.sessions_failed > 0 => {
                    tracing::info!(
                        sessions = report.sessions_cleaned,
                        files = report.files_deleted,
                        bytes = report.bytes_freed,
                        failed = report.sessions_failed,
                        "applied the audio retention policy"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "cannot apply the audio retention policy"),
            }
            std::thread::sleep(RETENTION_INTERVAL);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot start the audio retention thread");
    }
}

/// IPC commands the UI polls every few seconds; logged at `debug` to keep the log readable.
const POLLED_COMMANDS: &[&str] = &["display_thumbnail", "processing_status", "recording_status"];

/// Records each IPC command the UI calls (its name only: arguments may hold secrets).
fn log_command(command: &str) {
    if POLLED_COMMANDS.contains(&command) {
        tracing::debug!(command, "ipc command");
    } else {
        tracing::info!(command, "ipc command");
    }
}

/// Set once a requested exit has waited for the running recording (so it exits only once).
static EXIT_SETTLED: AtomicBool = AtomicBool::new(false);

/// Starts the application.
///
/// # Panics
///
/// Panics if the Tauri runtime fails to start.
pub fn run() {
    // Logging comes first so that a failure anywhere after this point leaves a trace.
    match resolve_layout() {
        Ok(layout) => logging::init(&logging::logs_dir(layout.root())),
        Err(error) => eprintln!("cannot resolve the data directory for logs: {error}"),
    }
    let builder = ipc_builder();
    let invoke_handler = builder.invoke_handler();

    tauri::Builder::default()
        // Must be the first plugin: a second launch brings the running window forward and
        // exits before it can touch the database or run the startup recovery.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!("a second Tan instance was started and exited; showing this window");
            tray::show_main_window(app);
        }))
        // The "save as" dialog of the Markdown export (the capability allows `save` only).
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(move |invoke| {
            log_command(invoke.message.command());
            invoke_handler(invoke)
        })
        .on_window_event(|window, event| {
            // A recording must outlive its window: while one runs, closing the window only
            // hides it (the tray icon stays and offers stop / show / quit).
            if let WindowEvent::CloseRequested { api, .. } = event {
                if commands::recording::is_busy(window.app_handle()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            builder.mount_events(app);
            let layout = resolve_layout()?;
            tracing::info!(data_dir = %layout.root().display(), "opening application data");
            let events = TauriRecordingEvents::new(app.handle().clone());
            let state = AppState::build(layout, events)?;

            // Settle recordings a crash left behind before anything can start a new one.
            match tauri::async_runtime::block_on(state.recording.recover_interrupted()) {
                Ok(recovered) => {
                    for session in recovered {
                        tracing::warn!(
                            session = %session.session_id,
                            status = %session.status,
                            tracks = session.tracks,
                            frames = session.frames,
                            "recovered an interrupted recording"
                        );
                    }
                }
                Err(error) => tracing::error!(%error, "cannot recover interrupted recordings"),
            }

            // A crash can also leave a session half processed: make it retryable.
            match tauri::async_runtime::block_on(state.process_session.recover_interrupted()) {
                Ok(0) => {}
                Ok(settled) => {
                    tracing::warn!(settled, "settled sessions whose processing was interrupted");
                }
                Err(error) => tracing::error!(%error, "cannot settle interrupted processing"),
            }

            let ui_language = tauri::async_runtime::block_on(state.get_settings.execute())
                .map_or(UiLanguage::Tr, |settings| settings.ui_language);
            let retention = state.apply_retention.clone();
            app.manage(state);
            spawn_retention(retention);
            // The tray is a convenience; the app works without it.
            if let Err(error) = tray::init(app.handle(), ui_language) {
                tracing::warn!(%error, "cannot create the tray icon");
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Tan application")
        .run(|app, event| {
            // Every exit path (tray Quit, Cmd+Q, OS logout) must let a running recording
            // finish: hold the exit, settle the session, then exit again.
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if !EXIT_SETTLED.load(Ordering::SeqCst) && commands::recording::is_busy(app) {
                    api.prevent_exit();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        commands::recording::stop_and_settle(&app).await;
                        // Whatever happened, the next exit request must go through.
                        EXIT_SETTLED.store(true, Ordering::SeqCst);
                        app.exit(code.unwrap_or(0));
                    });
                }
            }
        });
}
