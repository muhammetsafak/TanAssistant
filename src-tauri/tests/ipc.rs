//! Integration tests for the IPC surface. They live outside the lib so that the Windows
//! test executable gets the Common-Controls manifest (see `build.rs`).

use std::path::Path;

use tan_app_lib::{commands, export_bindings, BINDINGS_PATH};

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn app_version_matches_manifest() {
    assert_eq!(commands::app_version(), env!("CARGO_PKG_VERSION"));
}

/// Fails when `ui/src/ipc/bindings.ts` is stale. Regenerate it with
/// `TAN_UPDATE_BINDINGS=1 cargo test -p tan-app --test ipc`.
#[test]
fn bindings_are_up_to_date() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join(BINDINGS_PATH);

    if std::env::var_os("TAN_UPDATE_BINDINGS").is_some() {
        export_bindings(&committed).expect("failed to export TypeScript bindings");
        return;
    }

    let fresh = std::env::temp_dir().join(format!("tan-bindings-{}.ts", std::process::id()));
    export_bindings(&fresh).expect("failed to export TypeScript bindings");
    let fresh_text = std::fs::read_to_string(&fresh).expect("read fresh bindings");
    let _ = std::fs::remove_file(&fresh);

    let committed_text = std::fs::read_to_string(&committed)
        .expect("ui/src/ipc/bindings.ts is missing; run with TAN_UPDATE_BINDINGS=1");
    assert_eq!(
        normalize(&committed_text),
        normalize(&fresh_text),
        "ui/src/ipc/bindings.ts is stale; regenerate with TAN_UPDATE_BINDINGS=1"
    );
}

/// Each export refusal reaches the UI as `validation` with its own typed `exportProblem`;
/// other failures carry none.
#[test]
fn export_refusals_carry_a_typed_problem_code() {
    use tan_app_lib::commands::sessions::{ExportProblemCode, SessionsErrorCode, SessionsIpcError};
    use tan_application::sessions::{ExportDestinationProblem as Problem, SessionsError};

    let cases = [
        (Problem::NotAbsolute, ExportProblemCode::NotAbsolute),
        (Problem::ParentReference, ExportProblemCode::ParentReference),
        (Problem::NetworkPath, ExportProblemCode::NetworkPath),
        (Problem::MissingName, ExportProblemCode::MissingName),
        (
            Problem::InvalidExtension,
            ExportProblemCode::InvalidExtension,
        ),
        (
            Problem::NotDialogSelected,
            ExportProblemCode::NotDialogSelected,
        ),
        (Problem::InsideDataDir, ExportProblemCode::InsideDataDir),
        (Problem::FolderMissing, ExportProblemCode::FolderMissing),
        (Problem::AlreadyExists, ExportProblemCode::AlreadyExists),
    ];
    for (problem, expected) in cases {
        let error = SessionsIpcError::from(problem);
        assert_eq!(error.code, SessionsErrorCode::Validation);
        assert_eq!(error.export_problem, Some(expected));
        assert!(
            error.message.contains("export destination"),
            "{}",
            error.message
        );
    }
    let other = SessionsIpcError::from(SessionsError::NotFound("session"));
    assert_eq!(other.code, SessionsErrorCode::NotFound);
    assert_eq!(other.export_problem, None);

    // A session without audio is a validation error the UI tells apart by its own problem code.
    let no_audio = SessionsIpcError::from(SessionsError::NoAudio);
    assert_eq!(no_audio.code, SessionsErrorCode::Validation);
    assert_eq!(no_audio.export_problem, Some(ExportProblemCode::NoAudio));

    // So is a session that is still being recorded (not `busy`): the UI sees
    // `session_recording`, whichever layer raised it.
    let recording = SessionsIpcError::from(SessionsError::Recording);
    assert_eq!(recording.code, SessionsErrorCode::Validation);
    assert_eq!(
        recording.export_problem,
        Some(ExportProblemCode::SessionRecording)
    );

    // The audio export's own extension reason reaches the UI as `invalid_extension`.
    let wrong = SessionsIpcError::from(Problem::InvalidAudioExtension);
    assert_eq!(
        wrong.export_problem,
        Some(ExportProblemCode::InvalidExtension)
    );
    assert!(wrong.message.contains(".wav"), "{}", wrong.message);
}

/// The update rules are pure functions (no `AppHandle`), so they are tested here, outside the
/// lib (see the note at the top of this file).
mod updates {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tan_app_lib::commands::processing::{ProcessingErrorCode, ProcessingIpcError};
    use tan_app_lib::commands::recording::{RecordingErrorCode, RecordingIpcError};
    use tan_app_lib::commands::updates::{
        can_self_update_for, ensure_can_download, ensure_can_install, ensure_can_restart, gated,
        Activity, BlockedReason, DownloadContext, InstallContext, Slots, StartKind, UpdateError,
        UpdateInProgress, UpdaterState,
    };
    use tan_app_lib::error::{ErrorCode, IpcError};

    fn install_context() -> InstallContext {
        InstallContext {
            can_self_update: true,
            update_downloaded: true,
            activity: Activity::default(),
        }
    }

    #[test]
    fn install_is_allowed_when_nothing_is_running() {
        assert_eq!(ensure_can_install(install_context()), Ok(()));
    }

    #[test]
    fn install_is_blocked_while_recording() {
        let context = InstallContext {
            activity: Activity {
                recording: true,
                processing: false,
            },
            ..install_context()
        };
        assert_eq!(
            ensure_can_install(context),
            Err(UpdateError::Blocked(BlockedReason::Recording))
        );
    }

    #[test]
    fn install_is_blocked_while_processing() {
        let context = InstallContext {
            activity: Activity {
                recording: false,
                processing: true,
            },
            ..install_context()
        };
        assert_eq!(
            ensure_can_install(context),
            Err(UpdateError::Blocked(BlockedReason::Processing))
        );
    }

    #[test]
    fn install_needs_a_downloaded_update_and_a_self_updating_install() {
        let nothing = InstallContext {
            update_downloaded: false,
            ..install_context()
        };
        assert_eq!(ensure_can_install(nothing), Err(UpdateError::NotFound));
        // Unsupported wins: there is no point downloading on a `.deb`.
        let deb = InstallContext {
            can_self_update: false,
            activity: Activity {
                recording: true,
                processing: true,
            },
            ..install_context()
        };
        assert_eq!(ensure_can_install(deb), Err(UpdateError::Unsupported));
    }

    #[test]
    fn download_rules() {
        let ok = DownloadContext {
            can_self_update: true,
            update_found: true,
            download_running: false,
        };
        assert_eq!(ensure_can_download(ok), Ok(()));
        assert_eq!(
            ensure_can_download(DownloadContext {
                download_running: true,
                ..ok
            }),
            Err(UpdateError::Blocked(BlockedReason::DownloadRunning))
        );
        assert_eq!(
            ensure_can_download(DownloadContext {
                update_found: false,
                ..ok
            }),
            Err(UpdateError::NotFound)
        );
        assert_eq!(
            ensure_can_download(DownloadContext {
                can_self_update: false,
                ..ok
            }),
            Err(UpdateError::Unsupported)
        );
    }

    #[test]
    fn a_restart_after_the_install_is_held_back_when_busy() {
        let idle = Activity::default();
        assert_eq!(ensure_can_restart(idle), Ok(()));
        for busy in [
            Activity {
                recording: true,
                processing: false,
            },
            Activity {
                recording: false,
                processing: true,
            },
        ] {
            // The update is installed either way, so it is "pending", not "blocked".
            assert_eq!(ensure_can_restart(busy), Err(UpdateError::RestartPending));
        }
    }

    #[test]
    fn only_an_appimage_updates_itself_on_linux() {
        assert!(!can_self_update_for(true, false), "a .deb install");
        assert!(can_self_update_for(true, true), "an AppImage");
        assert!(can_self_update_for(false, false), "Windows or macOS");
        assert!(can_self_update_for(false, true));
    }

    #[test]
    fn errors_map_to_their_ipc_codes() {
        let cases = [
            (
                UpdateError::Blocked(BlockedReason::Recording),
                ErrorCode::UpdateBlocked,
            ),
            (UpdateError::RestartPending, ErrorCode::UpdateRestartPending),
            (UpdateError::NotFound, ErrorCode::UpdateNotFound),
            (UpdateError::Unsupported, ErrorCode::UpdateUnsupported),
            (UpdateError::Failed, ErrorCode::UpdateFailed),
        ];
        for (error, code) in cases {
            assert_eq!(IpcError::from(error).code, code);
        }
    }

    // The tests use a plain string as the "update": `Slots` is generic over it.
    fn slots_with_found(version: &str) -> Slots<String> {
        let mut slots = Slots::default();
        slots.record_found(version, format!("update {version}"));
        slots
    }

    #[test]
    fn a_failed_download_clears_the_running_flag_and_keeps_no_bundle() {
        let mut slots = slots_with_found("1.1.0");
        assert_eq!(
            slots.start_download(true),
            Ok(Some("update 1.1.0".to_owned()))
        );
        // While it runs, a second download is refused.
        assert_eq!(
            slots.start_download(true),
            Err(UpdateError::Blocked(BlockedReason::DownloadRunning))
        );
        slots.finish_download("1.1.0", None);
        assert!(!slots.has_package());
        // The flag is clear again: the download can be retried.
        assert_eq!(
            slots.start_download(true),
            Ok(Some("update 1.1.0".to_owned()))
        );
    }

    #[test]
    fn a_finished_download_is_not_repeated_and_a_newer_version_drops_it() {
        let mut slots = slots_with_found("1.1.0");
        slots.start_download(true).unwrap();
        slots.finish_download("1.1.0", Some(("update 1.1.0".to_owned(), vec![1, 2, 3])));
        assert!(slots.has_package());
        assert_eq!(slots.start_download(true), Ok(None), "already downloaded");

        // A check that finds the same version keeps the bundle...
        slots.record_found("1.1.0", "update 1.1.0".to_owned());
        assert!(slots.has_package());
        // ...one that finds another version drops it.
        slots.record_found("1.2.0", "update 1.2.0".to_owned());
        assert!(!slots.has_package());
        assert_eq!(
            slots.start_download(true),
            Ok(Some("update 1.2.0".to_owned()))
        );
    }

    #[test]
    fn downloads_need_a_found_update_and_a_self_updating_install() {
        let mut empty: Slots<String> = Slots::default();
        assert_eq!(empty.start_download(true), Err(UpdateError::NotFound));
        let mut slots = slots_with_found("1.1.0");
        assert_eq!(slots.start_download(false), Err(UpdateError::Unsupported));
        slots.clear_found();
        assert_eq!(slots.start_download(true), Err(UpdateError::NotFound));
    }

    #[test]
    fn a_failed_install_keeps_the_bundle_and_an_installed_one_waits_for_the_restart() {
        let mut slots = slots_with_found("1.1.0");
        slots.start_download(true).unwrap();
        slots.finish_download("1.1.0", Some(("update 1.1.0".to_owned(), vec![9])));

        let package = slots.take_for_install().expect("a bundle");
        assert!(!slots.has_package(), "taken for the install");
        slots.restore_after_failed_install(package);
        assert!(slots.has_package(), "the install can be retried");

        let _package = slots.take_for_install().expect("a bundle");
        assert!(!slots.is_installed());
        slots.mark_installed();
        assert!(slots.is_installed(), "only the restart is left");
        assert!(slots.has_package());
        assert!(
            slots.take_for_install().is_none(),
            "nothing to install again"
        );
    }

    #[test]
    fn an_installed_update_is_forgotten_by_a_new_download_only() {
        let mut slots = slots_with_found("1.1.0");
        slots.start_download(true).unwrap();
        slots.finish_download("1.1.0", Some(("update 1.1.0".to_owned(), vec![1])));
        let _package = slots.take_for_install().unwrap();
        slots.mark_installed();
        // A repeated "install" call branches on this: it must go straight to the restart check.
        assert!(slots.is_installed());
        // A check that finds the same update again does not undo it...
        slots.record_found("1.1.0", "update 1.1.0".to_owned());
        assert!(slots.is_installed());
        // ...a finished download of a bundle does (it is a new install to make).
        slots.start_download(true).unwrap();
        slots.finish_download("1.1.0", Some(("update 1.1.0".to_owned(), vec![2])));
        assert!(!slots.is_installed());
    }

    fn downloaded_slots() -> Slots<String> {
        let mut slots = slots_with_found("1.1.0");
        slots.start_download(true).unwrap();
        slots.finish_download("1.1.0", Some(("update 1.1.0".to_owned(), vec![1])));
        slots
    }

    #[test]
    fn without_an_install_every_start_is_admitted_as_before() {
        let mut slots: Slots<String> = Slots::default();
        assert!(!slots.is_install_gate_closed());
        assert_eq!(slots.starts_in_flight(), Activity::default());
        assert_eq!(slots.admit_start(StartKind::Recording), Ok(()));
        assert_eq!(slots.admit_start(StartKind::Processing), Ok(()));
        assert_eq!(slots.admit_start(StartKind::Processing), Ok(()));
        assert_eq!(
            slots.starts_in_flight(),
            Activity {
                recording: true,
                processing: true
            }
        );
        slots.end_start(StartKind::Recording);
        slots.end_start(StartKind::Processing);
        assert_eq!(
            slots.starts_in_flight(),
            Activity {
                recording: false,
                processing: true
            },
            "one processing run is still going"
        );
        slots.end_start(StartKind::Processing);
        assert_eq!(slots.starts_in_flight(), Activity::default());
        // Ending more than was admitted does not wrap around.
        slots.end_start(StartKind::Recording);
        assert_eq!(slots.starts_in_flight(), Activity::default());
    }

    #[test]
    fn a_closed_install_gate_refuses_every_start_and_leaves_the_counts_alone() {
        let mut slots = downloaded_slots();
        slots.admit_start(StartKind::Recording).unwrap();
        slots.close_install_gate().unwrap();
        assert!(slots.is_install_gate_closed());
        for kind in [StartKind::Recording, StartKind::Processing] {
            assert_eq!(slots.admit_start(kind), Err(UpdateInProgress));
        }
        // What was admitted before the gate closed is still seen (the install counts it as
        // activity); nothing refused was counted.
        assert_eq!(
            slots.starts_in_flight(),
            Activity {
                recording: true,
                processing: false
            }
        );
        slots.end_start(StartKind::Recording);
        assert_eq!(slots.starts_in_flight(), Activity::default());
    }

    #[test]
    fn a_second_install_cannot_take_the_gate_while_one_runs() {
        let mut slots = downloaded_slots();
        assert_eq!(slots.close_install_gate(), Ok(()));
        assert_eq!(
            slots.close_install_gate(),
            Err(UpdateError::Blocked(BlockedReason::InstallRunning))
        );
        // A download would reset what the install is about to mark.
        slots.record_found("1.2.0", "update 1.2.0".to_owned());
        assert_eq!(
            slots.start_download(true),
            Err(UpdateError::Blocked(BlockedReason::InstallRunning))
        );
    }

    #[test]
    fn a_refused_install_opens_the_gate_again() {
        let mut slots = downloaded_slots();
        slots.close_install_gate().unwrap();
        slots.open_install_gate();
        assert!(!slots.is_install_gate_closed());
        assert_eq!(slots.admit_start(StartKind::Recording), Ok(()));
        assert_eq!(slots.close_install_gate(), Ok(()), "and it can close again");
    }

    #[test]
    fn a_failed_install_opens_the_gate_and_keeps_the_bundle() {
        let mut slots = downloaded_slots();
        slots.close_install_gate().unwrap();
        let package = slots.take_for_install().expect("a bundle");
        assert_eq!(
            slots.admit_start(StartKind::Processing),
            Err(UpdateInProgress)
        );

        slots.restore_after_failed_install(package);
        assert!(!slots.is_install_gate_closed(), "the gate is open again");
        assert!(slots.has_package(), "the install can be retried");
        assert_eq!(slots.admit_start(StartKind::Recording), Ok(()));
        assert_eq!(slots.admit_start(StartKind::Processing), Ok(()));
    }

    #[test]
    fn a_successful_install_keeps_the_gate_closed_until_the_restart() {
        let mut slots = downloaded_slots();
        slots.close_install_gate().unwrap();
        let _package = slots.take_for_install().unwrap();
        slots.mark_installed();
        assert!(slots.is_install_gate_closed());
        assert_eq!(
            slots.admit_start(StartKind::Recording),
            Err(UpdateInProgress)
        );
        assert_eq!(
            slots.admit_start(StartKind::Processing),
            Err(UpdateInProgress)
        );
        // A repeated install call goes straight to the restart check and does not reopen it.
        assert!(slots.is_installed());
        assert!(slots.is_install_gate_closed());
    }

    #[test]
    fn a_new_download_after_an_install_lets_the_next_install_close_the_gate_again() {
        let mut slots = downloaded_slots();
        slots.close_install_gate().unwrap();
        let _package = slots.take_for_install().unwrap();
        slots.mark_installed();

        slots.record_found("1.2.0", "update 1.2.0".to_owned());
        slots.start_download(true).unwrap();
        slots.finish_download("1.2.0", Some(("update 1.2.0".to_owned(), vec![2])));
        assert!(!slots.is_installed());
        assert!(!slots.is_install_gate_closed());
        assert_eq!(slots.close_install_gate(), Ok(()));
    }

    #[test]
    fn a_refused_start_says_update_in_progress_in_both_command_families() {
        let refused = UpdateInProgress;
        assert_eq!(
            RecordingIpcError::from(refused).code,
            RecordingErrorCode::UpdateInProgress
        );
        let processing = ProcessingIpcError::from(refused);
        assert_eq!(processing.code, ProcessingErrorCode::UpdateInProgress);
        assert!(processing.transcription_code.is_none());
        assert!(RecordingIpcError::from(refused).message.contains("update"));
    }

    #[test]
    fn a_permit_counts_as_activity_until_it_is_dropped_and_is_refused_once_the_gate_closed() {
        let state = UpdaterState::default();
        let recording = state.admit(StartKind::Recording).expect("admitted");
        {
            let _run = state.admit(StartKind::Processing).expect("admitted");
            assert_eq!(
                state.starts_in_flight(),
                Activity {
                    recording: true,
                    processing: true
                }
            );
        }
        assert_eq!(
            state.starts_in_flight(),
            Activity {
                recording: true,
                processing: false
            }
        );

        // The install closes the gate while the recording is still starting: it is refused
        // nothing, but it is seen, and nothing new gets in.
        state.close_install_gate().unwrap();
        assert!(state.admit(StartKind::Recording).is_err());
        assert!(state.admit(StartKind::Processing).is_err());
        assert!(state.starts_in_flight().recording);
        drop(recording);
        assert_eq!(state.starts_in_flight(), Activity::default());
    }

    /// A start that reports whether the permit was held while it ran.
    // `async` on purpose: the body must run only when the future is polled.
    #[allow(clippy::unused_async)]
    async fn observed_start(
        state: &UpdaterState,
        ran: &AtomicBool,
    ) -> Result<Activity, UpdateInProgress> {
        ran.store(true, Ordering::SeqCst);
        Ok(state.starts_in_flight())
    }

    #[test]
    fn gated_holds_the_permit_for_the_whole_start_and_releases_it_after() {
        let state = UpdaterState::default();
        let ran = AtomicBool::new(false);
        let during = tauri::async_runtime::block_on(gated(&state, StartKind::Processing, {
            observed_start(&state, &ran)
        }))
        .expect("admitted");
        assert!(ran.load(Ordering::SeqCst));
        assert_eq!(
            during,
            Activity {
                recording: false,
                processing: true
            },
            "the run counts as activity while it lasts"
        );
        assert_eq!(state.starts_in_flight(), Activity::default(), "released");

        let during = tauri::async_runtime::block_on(gated(&state, StartKind::Recording, {
            observed_start(&state, &ran)
        }))
        .expect("admitted");
        assert!(during.recording && !during.processing);
        assert_eq!(state.starts_in_flight(), Activity::default());
    }

    #[test]
    fn gated_refuses_without_running_the_start_while_the_gate_is_closed() {
        let state = UpdaterState::default();
        state.close_install_gate().unwrap();
        for kind in [StartKind::Recording, StartKind::Processing] {
            let ran = AtomicBool::new(false);
            let result = tauri::async_runtime::block_on(gated(&state, kind, {
                observed_start(&state, &ran)
            }));
            assert_eq!(result, Err(UpdateInProgress));
            assert!(!ran.load(Ordering::SeqCst), "the start must not run");
        }
        assert_eq!(state.starts_in_flight(), Activity::default());
    }

    #[test]
    fn a_blocked_install_names_its_reason() {
        let error = UpdateError::Blocked(BlockedReason::InstallRunning);
        assert_eq!(IpcError::from(error).code, ErrorCode::UpdateBlocked);
    }
}
