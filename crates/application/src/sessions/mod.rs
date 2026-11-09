//! Session use cases beyond recording: browsing, searching, exporting, retention and deletion.
//!
//! Every error type here is safe to show to the user.

mod audio_export;
mod export;
mod queries;
mod retention;

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tan_domain::{ActionItemId, EpochMs, SessionId, SessionStatus, TrackKind};

use crate::error::RepositoryError;
use crate::ports::{NoteRepository, SessionRepository};

pub use audio_export::ExportSessionAudio;
pub use export::{
    render_markdown, resolve_export_destination, resolve_export_destination_as, ExportDestination,
    ExportDestinationProblem, ExportExtension, ExportSessionMarkdown, EXPORT_FILE_NAME,
    FRAMES_DIR_SUFFIX,
};
pub use queries::{
    AudioTrackState, GetSessionDetail, ListSessions, NoteWithItems, Page, SearchSessions,
    SessionDetail, SessionFilter, SessionListItem, SessionPage, SessionSearchResult,
    DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
pub use retention::{ApplyRetention, GetStorageUsage, RetentionReport};

/// Disk space used by the application's data, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    /// Everything under the data directory (database included).
    pub total_bytes: u64,
    /// Audio files of all sessions.
    pub audio_bytes: u64,
    /// Frame images of all sessions.
    pub frame_bytes: u64,
    /// Downloaded whisper models.
    pub model_bytes: u64,
}

/// The files of sessions: frame images, audio, exports. Paths handed in are relative to the
/// session directory, as stored in the database; an implementation must refuse one that leaves
/// the session directory.
pub trait SessionFileStore: Send + Sync {
    /// The bytes of a frame image.
    fn read_frame(&self, session_id: &SessionId, relative_path: &str) -> io::Result<Vec<u8>>;

    /// Whether an audio file exists.
    fn audio_exists(&self, session_id: &SessionId, relative_path: &str) -> bool;

    /// Deletes an audio file and returns the bytes freed. A file that is already gone frees 0
    /// and is not an error, so a run that was interrupted halfway can simply be repeated.
    fn delete_audio(&self, session_id: &SessionId, relative_path: &str) -> io::Result<u64>;

    /// Deletes the session's whole folder (a missing folder is fine).
    fn remove_session(&self, session_id: &SessionId) -> io::Result<()>;

    /// Writes `content` to `file_name` in the session folder (replacing it) and returns the
    /// absolute path. Stored frame paths are relative to the same folder, so image links in
    /// the file resolve.
    fn write_export(
        &self,
        session_id: &SessionId,
        file_name: &str,
        content: &str,
    ) -> io::Result<PathBuf>;

    /// Creates `dir` and its missing parents (a folder the user chose for an export).
    fn create_export_dir(&self, dir: &Path) -> io::Result<()>;

    /// Writes `content` to the absolute path `destination` (replacing the file) and never
    /// leaves a half-written file. The parent folder must exist already.
    fn write_export_to(&self, destination: &Path, content: &[u8]) -> io::Result<()>;

    /// Measures disk usage.
    fn measure_usage(&self) -> io::Result<StorageUsage>;
}

/// Which audio files were removed by the retention policy. The track rows stay (so the UI can
/// say "audio deleted") and are marked with the time.
pub trait AudioRetentionRepository: Send + Sync {
    /// The session's tracks whose audio file was deleted, with the deletion time.
    fn deleted_tracks(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = Result<Vec<(TrackKind, EpochMs)>, RepositoryError>> + Send;

    /// Marks a track's audio as deleted.
    fn mark_audio_deleted(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        at: EpochMs,
    ) -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

/// Failure of a session use case.
#[derive(Debug)]
pub enum SessionsError {
    /// The addressed entity does not exist.
    NotFound(&'static str),
    /// The input was rejected.
    Invalid(String),
    /// The export destination was rejected, for the typed reason.
    ExportDestination(ExportDestinationProblem),
    /// The session is in a state that does not allow the operation.
    Busy(SessionStatus),
    /// The session has no audio that can be exported (none recorded, deleted by the retention
    /// policy, or the files are gone).
    NoAudio,
    /// The session is still being recorded, so its audio is not complete yet.
    Recording,
    /// A file operation failed.
    Files(io::Error),
    /// The database failed.
    Storage(RepositoryError),
}

impl fmt::Display for SessionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(entity) => write!(f, "{entity} not found"),
            Self::Invalid(why) => write!(f, "invalid input: {why}"),
            Self::ExportDestination(problem) => {
                write!(f, "invalid input: export destination: {problem}")
            }
            Self::Busy(status) => write!(f, "the session cannot be changed while it is {status}"),
            Self::NoAudio => f.write_str("invalid input: the session has no audio to export"),
            Self::Recording => f.write_str("invalid input: the session is still being recorded"),
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Files(_) => f.write_str("a file operation failed"),
            Self::Storage(_) => f.write_str("the database operation failed"),
        }
    }
}

impl Error for SessionsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Files(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RepositoryError> for SessionsError {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::NotFound { entity, .. } => Self::NotFound(entity),
            RepositoryError::Invalid(why) => Self::Invalid(why),
            other => Self::Storage(other),
        }
    }
}

impl From<io::Error> for SessionsError {
    fn from(error: io::Error) -> Self {
        Self::Files(error)
    }
}

/// Result alias for session use cases.
pub type SessionsResult<T> = Result<T, SessionsError>;

/// Marks an action item done or not done.
pub struct ToggleActionItem<N> {
    notes: Arc<N>,
}

impl<N: NoteRepository> ToggleActionItem<N> {
    /// Creates the use case.
    #[must_use]
    pub fn new(notes: Arc<N>) -> Self {
        Self { notes }
    }

    /// Sets the item's `done` flag (idempotent).
    pub async fn execute(&self, id: &ActionItemId, done: bool) -> SessionsResult<()> {
        Ok(self.notes.set_action_item_done(id, done).await?)
    }
}

/// Deletes a session with everything derived from it: its files, then the database rows
/// (frames, transcript, note, action items, job, search index).
///
/// A session that is being recorded or processed is refused. Files go first: if removing them
/// fails the session is still there and the deletion can be repeated.
pub struct DeleteSession<S, W> {
    sessions: Arc<S>,
    files: Arc<W>,
}

impl<S: SessionRepository, W: SessionFileStore> DeleteSession<S, W> {
    /// Creates the use case.
    #[must_use]
    pub fn new(sessions: Arc<S>, files: Arc<W>) -> Self {
        Self { sessions, files }
    }

    /// Deletes the session.
    pub async fn execute(&self, id: &SessionId) -> SessionsResult<()> {
        let session = self
            .sessions
            .get(id)
            .await?
            .ok_or(SessionsError::NotFound("session"))?;
        if matches!(
            session.status(),
            SessionStatus::Recording | SessionStatus::Processing
        ) {
            return Err(SessionsError::Busy(session.status()));
        }
        self.files.remove_session(id)?;
        self.sessions.delete(id).await?;
        Ok(())
    }
}
