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
