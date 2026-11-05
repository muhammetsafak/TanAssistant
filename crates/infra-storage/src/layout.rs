//! On-disk layout of the application data directory.
//!
//! ```text
//! <data>/tan.db
//! <data>/sessions/<session-id>/audio/
//! <data>/sessions/<session-id>/frames/
//! ```

use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use tan_domain::SessionId;

const DATABASE_FILE: &str = "tan.db";
const SESSIONS_DIR: &str = "sessions";
const AUDIO_DIR: &str = "audio";
const FRAMES_DIR: &str = "frames";

/// Resolves every path the application stores data at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLayout {
    root: PathBuf,
}

impl DataLayout {
    /// A layout rooted at an explicit directory (tests, portable installs, user override).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The OS-appropriate application data directory (for example
    /// `%APPDATA%\muhammetsafak\Tan\data` on Windows). `None` when the OS reports no home
    /// directory.
    #[must_use]
    pub fn for_current_os() -> Option<Self> {
        ProjectDirs::from("dev", "muhammetsafak", "Tan")
            .map(|dirs| Self::new(dirs.data_dir().to_path_buf()))
    }

    /// The data directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the SQLite database file.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.root.join(DATABASE_FILE)
    }

    /// Directory holding all session folders.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join(SESSIONS_DIR)
    }

    /// Folder of one session.
    ///
    /// The id becomes a path segment, so anything other than ASCII letters, digits, `-` and
    /// `_` is rejected; that rules out separators and `..`.
    pub fn session_dir(&self, id: &SessionId) -> io::Result<PathBuf> {
        let name = id.as_str();
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session id is not usable as a directory name",
            ));
        }
        Ok(self.sessions_dir().join(name))
    }

    /// Audio folder of a session.
    pub fn audio_dir(&self, id: &SessionId) -> io::Result<PathBuf> {
        Ok(self.session_dir(id)?.join(AUDIO_DIR))
    }

    /// Frames folder of a session.
    pub fn frames_dir(&self, id: &SessionId) -> io::Result<PathBuf> {
        Ok(self.session_dir(id)?.join(FRAMES_DIR))
    }

    /// Creates the data directory.
    pub fn ensure_root(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    /// Creates the audio and frames folders of a session (and their parents).
    pub fn ensure_session_dirs(&self, id: &SessionId) -> io::Result<()> {
        std::fs::create_dir_all(self.audio_dir(id)?)?;
        std::fs::create_dir_all(self.frames_dir(id)?)
    }

    /// Removes a session's folder with all its files. A missing folder is not an error.
    pub fn remove_session_dir(&self, id: &SessionId) -> io::Result<()> {
        match std::fs::remove_dir_all(self.session_dir(id)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DataLayout;
    use tan_domain::SessionId;

    #[test]
    fn paths_follow_the_documented_layout() {
        let layout = DataLayout::new("/data");
        let id = SessionId::new("0190-abc_1");
        assert!(layout.database_path().ends_with("tan.db"));
        assert!(layout
            .audio_dir(&id)
            .unwrap()
            .ends_with("sessions/0190-abc_1/audio"));
        assert!(layout
            .frames_dir(&id)
            .unwrap()
            .ends_with("sessions/0190-abc_1/frames"));
    }

    #[test]
    fn unsafe_session_ids_are_rejected() {
        let layout = DataLayout::new("/data");
        for bad in ["", "..", "../x", "a/b", "a\\b", "a b", "c:evil"] {
            assert!(layout.session_dir(&SessionId::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn session_dirs_are_created_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        let id = SessionId::new("s1");
        layout.ensure_session_dirs(&id).unwrap();
        assert!(layout.audio_dir(&id).unwrap().is_dir());
        assert!(layout.frames_dir(&id).unwrap().is_dir());
        layout.remove_session_dir(&id).unwrap();
        assert!(!layout.session_dir(&id).unwrap().exists());
        layout.remove_session_dir(&id).unwrap(); // idempotent
    }
}
