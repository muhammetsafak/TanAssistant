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
    use tan_app_lib::commands::updates::{
        any_running, can_self_update_for, ensure_can_download, ensure_can_install,
        ensure_can_restart, Activity, BlockedReason, DownloadContext, InstallContext, Slots,
        UpdateError,
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
    fn a_restart_after_the_install_is_held_back_when_busy_or_unsure() {
        let idle = Activity::default();
        assert_eq!(ensure_can_restart(Ok(idle)), Ok(()));
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
            assert_eq!(
                ensure_can_restart(Ok(busy)),
                Err(UpdateError::RestartPending)
            );
        }
        // The activity could not be read: unknown, so the restart waits (fail closed). The
        // update is installed either way, so it is "pending", not "failed".
        assert_eq!(
            ensure_can_restart(Err(UpdateError::Failed)),
            Err(UpdateError::RestartPending)
        );
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

    /// A fake paged list: `items` are served `page_size` at a time from the asked offset.
    fn paged(
        items: Vec<u32>,
        page_size: usize,
    ) -> impl FnMut(usize) -> std::future::Ready<Result<(Vec<u32>, usize), ()>> {
        move |offset| {
            let page = items.iter().skip(offset).take(page_size).copied().collect();
            std::future::ready(Ok((page, items.len())))
        }
    }

    #[test]
    fn any_running_walks_every_page() {
        let run = |items, page_size, running: u32| {
            tauri::async_runtime::block_on(any_running(paged(items, page_size), |&id| {
                id == running
            }))
        };
        // Found on the last page.
        assert_eq!(run(vec![1, 2, 3, 4, 5], 2, 5), Ok(true));
        // Found on the first page.
        assert_eq!(run(vec![1, 2, 3, 4, 5], 2, 1), Ok(true));
        // Not there at all, with a page size that does not divide the total.
        assert_eq!(run(vec![1, 2, 3, 4, 5], 2, 9), Ok(false));
        // No sessions in the status.
        assert_eq!(run(vec![], 2, 1), Ok(false));
    }

    #[test]
    fn any_running_stops_on_an_empty_page_and_passes_errors_on() {
        // A list that shrinks under the walk (total says 10, the page is empty) must not loop.
        let shrinking = |_offset: usize| std::future::ready(Ok::<_, ()>((Vec::<u32>::new(), 10)));
        assert_eq!(
            tauri::async_runtime::block_on(any_running(shrinking, |_| true)),
            Ok(false)
        );
        let failing = |_offset: usize| std::future::ready(Err::<(Vec<u32>, usize), _>("boom"));
        assert_eq!(
            tauri::async_runtime::block_on(any_running(failing, |_| true)),
            Err("boom")
        );
    }
}
