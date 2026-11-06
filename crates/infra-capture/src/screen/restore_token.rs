//! The `ScreenCast` portal restore token, kept in the application data directory.
//!
//! With `persist_mode` set the portal hands out a token after the user picked a source; giving
//! it back in the next session's `SelectSources` call lets the portal restore that choice
//! without showing the dialog again. The token only lets *this application* start a screen
//! cast the user already approved (and the user can revoke it in the desktop settings), but it
//! is still kept readable by the owner only.
//!
//! No dependency on the portal crates, so it can be unit tested on any OS; it is only compiled
//! on Linux (and in tests).
// Only the Linux portal adapter calls these; on other OSes just the tests reach them.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Environment variable that moves the data directory (same one the composition root reads).
pub(super) const DATA_DIR_ENV: &str = "TAN_DATA_DIR";

/// File name of the token inside the data directory.
const TOKEN_FILE: &str = "screencast-restore-token";

/// Longest token accepted. Portals issue UUID-like strings; anything much longer is not one.
const MAX_TOKEN_LEN: usize = 256;

/// The data directory: `TAN_DATA_DIR` when it is set and not empty, else `fallback`.
pub(super) fn resolve_data_dir(
    env_value: Option<OsString>,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match env_value.filter(|value| !value.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => fallback(),
    }
}

/// Whether `token` looks like a portal token: non-empty, short, printable ASCII without
/// whitespace.
fn is_valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_LEN && token.chars().all(|c| c.is_ascii_graphic())
}

/// Reads and writes the restore token file.
#[derive(Debug, Clone)]
pub(super) struct RestoreTokenStore {
    path: PathBuf,
}

impl RestoreTokenStore {
    /// The store of the application whose data directory is `data_dir`.
    pub(super) fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(TOKEN_FILE),
        }
    }

    /// The saved token, or `None` when there is none or the file does not hold a valid one.
    pub(super) fn load(&self) -> Option<String> {
        let text = fs::read_to_string(&self.path).ok()?;
        let token = text.trim();
        is_valid_token(token).then(|| token.to_owned())
    }

    /// Replaces the saved token. The file is written next to its final place and renamed, so a
    /// crash never leaves a half-written token, and it is readable by the owner only (Unix).
    pub(super) fn save(&self, token: &str) -> io::Result<()> {
        if !is_valid_token(token) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the portal returned a restore token that is not usable",
            ));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self.path.with_extension("tmp");
        let written = (|| -> io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(token.as_bytes())?;
            file.sync_all()
        })();
        match written.and_then(|()| fs::rename(&temp, &self.path)) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temp);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_token_is_loaded_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::in_data_dir(dir.path());
        assert_eq!(store.load(), None, "nothing saved yet");
        store.save("0d6f1c0e-3a52-4c3f-9d3b-1a2b3c4d5e6f").unwrap();
        assert_eq!(
            store.load().as_deref(),
            Some("0d6f1c0e-3a52-4c3f-9d3b-1a2b3c4d5e6f")
        );
        store.save("second").unwrap();
        assert_eq!(store.load().as_deref(), Some("second"), "replaced");
    }

    #[test]
    fn the_data_directory_is_created_and_no_temp_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("nested").join("data");
        let store = RestoreTokenStore::in_data_dir(&data_dir);
        store.save("token").unwrap();
        let names: Vec<_> = fs::read_dir(&data_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [TOKEN_FILE]);
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::in_data_dir(dir.path());
        store.save("token").unwrap();
        let mode = fs::metadata(dir.path().join(TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn unusable_tokens_are_neither_saved_nor_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::in_data_dir(dir.path());
        let too_long = "x".repeat(MAX_TOKEN_LEN + 1);
        for bad in ["", "has space", "line\nbreak", too_long.as_str()] {
            let error = store.save(bad).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert_eq!(store.load(), None);

        fs::write(dir.path().join(TOKEN_FILE), "not a token\nat all").unwrap();
        assert_eq!(store.load(), None, "a corrupt file is ignored");
        fs::write(dir.path().join(TOKEN_FILE), "  padded-token \n").unwrap();
        assert_eq!(store.load().as_deref(), Some("padded-token"));
    }

    #[test]
    fn the_environment_variable_wins_over_the_fallback() {
        let fallback = || Some(PathBuf::from("/fallback"));
        assert_eq!(
            resolve_data_dir(Some(OsString::from("/from/env")), fallback),
            Some(PathBuf::from("/from/env"))
        );
        assert_eq!(
            resolve_data_dir(Some(OsString::new()), fallback),
            Some(PathBuf::from("/fallback"))
        );
        assert_eq!(
            resolve_data_dir(None, fallback),
            Some(PathBuf::from("/fallback"))
        );
        assert_eq!(resolve_data_dir(None, || None), None);
    }
}
