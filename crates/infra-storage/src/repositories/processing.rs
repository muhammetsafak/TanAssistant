//! Processing state in SQLite and the files of sessions on disk.
//!
//! * [`SqliteProcessingRepository`] stores processing jobs ([`ProcessingJobRepository`]) and
//!   remembers which audio files the retention policy deleted ([`AudioRetentionRepository`]).
//! * [`DataDirFiles`] is the [`SessionFileStore`] over the data directory: frame images, audio
//!   deletion, the Markdown export and the disk-usage report.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use tan_application::processing::ProcessingJobRepository;
use tan_application::sessions::{AudioRetentionRepository, SessionFileStore, StorageUsage};
use tan_application::RepositoryError;
use tan_domain::{
    EpochMs, ProcessingJob, ProcessingJobRecord, ProcessingStep, SessionId, StepState, TrackKind,
};

use super::{parse_column, require_changed};
use crate::db::{Database, DbError};
use crate::layout::DataLayout;

/// SQLite-backed [`ProcessingJobRepository`] and [`AudioRetentionRepository`].
#[derive(Debug, Clone)]
pub struct SqliteProcessingRepository {
    db: Database,
}

impl SqliteProcessingRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl ProcessingJobRepository for SqliteProcessingRepository {
    async fn get(&self, session_id: &SessionId) -> Result<Option<ProcessingJob>, RepositoryError> {
        self.db.with_conn(|conn| {
            let header = conn
                .query_row(
                    "SELECT created_at, updated_at, finished_at FROM processing_jobs
                     WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |row| {
                        Ok((
                            EpochMs::new(row.get(0)?),
                            EpochMs::new(row.get(1)?),
                            row.get::<_, Option<i64>>(2)?.map(EpochMs::new),
                        ))
                    },
                )
                .optional()?;
            let Some((created_at, updated_at, finished_at)) = header else {
                return Ok(None);
            };

            let mut stmt = conn
                .prepare("SELECT step, state, error FROM processing_steps WHERE session_id = ?1")?;
            let rows = stmt
                .query_map(params![session_id.as_str()], |row| {
                    Ok((
                        parse_column::<ProcessingStep>(row, 0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let corrupt = |why: String| DbError::from(RepositoryError::CorruptData(why));
            let mut steps = Vec::with_capacity(rows.len());
            for (step, tag, error) in rows {
                let state =
                    StepState::from_parts(&tag, error).map_err(|e| corrupt(e.to_string()))?;
                steps.push((step, state));
            }
            ProcessingJob::restore(ProcessingJobRecord {
                session_id: session_id.clone(),
                steps,
                created_at,
                updated_at,
                finished_at,
            })
            .map(Some)
            .map_err(|e| corrupt(e.to_string()))
        })
    }

    async fn save(&self, job: &ProcessingJob) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            let id = job.session_id().as_str();
            tx.execute(
                "INSERT INTO processing_jobs (session_id, created_at, updated_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (session_id) DO UPDATE SET
                     created_at = excluded.created_at,
                     updated_at = excluded.updated_at,
                     finished_at = excluded.finished_at",
                params![
                    id,
                    job.created_at().get(),
                    job.updated_at().get(),
                    job.finished_at().map(EpochMs::get),
                ],
            )?;
            tx.execute(
                "DELETE FROM processing_steps WHERE session_id = ?1",
                params![id],
            )?;
            for (step, state) in job.steps() {
                let error = match state {
                    StepState::Failed { error } => Some(error.as_str()),
                    _ => None,
                };
                tx.execute(
                    "INSERT INTO processing_steps (session_id, step, state, error)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id, step.as_str(), state.tag(), error],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }
}

impl AudioRetentionRepository for SqliteProcessingRepository {
    async fn deleted_tracks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<(TrackKind, EpochMs)>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, deleted_at FROM audio_tracks
                 WHERE session_id = ?1 AND deleted_at IS NOT NULL ORDER BY kind",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |row| {
                Ok((
                    parse_column::<TrackKind>(row, 0)?,
                    EpochMs::new(row.get(1)?),
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn mark_audio_deleted(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        at: EpochMs,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE audio_tracks SET deleted_at = ?3 WHERE session_id = ?1 AND kind = ?2",
                params![session_id.as_str(), kind.as_str(), at.get()],
            )?;
            require_changed(changed, "audio track", session_id.as_str())
        })
    }
}

// ---------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------

/// Largest frame image the pipeline reads into memory. Frames are screenshots stored as PNG;
/// anything bigger is not something a vision model accepts anyway.
const MAX_FRAME_BYTES: u64 = 25 * 1024 * 1024;

/// The session folder that holds audio files.
const AUDIO_DIR: &str = "audio";

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

/// Whether `relative` is a plain relative path (no `..`, no root, no drive prefix, no `.`
/// tricks) and returns its components.
fn plain_components(relative: &str) -> io::Result<Vec<&std::ffi::OsStr>> {
    let mut parts = Vec::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            _ => {
                return Err(invalid(
                    "the path must be relative and stay inside the session",
                ))
            }
        }
    }
    if parts.is_empty() {
        return Err(invalid("the path is empty"));
    }
    Ok(parts)
}

/// Size of everything under `dir`, not following symbolic links. Entries that cannot be read
/// count as zero: a usage report is an estimate, not an audit.
fn directory_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// [`SessionFileStore`] over the application data directory (see [`DataLayout`]).
#[derive(Debug, Clone)]
pub struct DataDirFiles {
    layout: DataLayout,
}

impl DataDirFiles {
    /// Creates the file store for `layout`.
    #[must_use]
    pub fn new(layout: DataLayout) -> Self {
        Self { layout }
    }

    /// The absolute path of a path relative to the session folder; refuses anything that could
    /// leave it.
    fn resolve(&self, session_id: &SessionId, relative_path: &str) -> io::Result<PathBuf> {
        let mut path = self.layout.session_dir(session_id)?;
        for part in plain_components(relative_path)? {
            path.push(part);
        }
        Ok(path)
    }
}

impl SessionFileStore for DataDirFiles {
    fn read_frame(&self, session_id: &SessionId, relative_path: &str) -> io::Result<Vec<u8>> {
        let path = self.resolve(session_id, relative_path)?;
        if fs::metadata(&path)?.len() > MAX_FRAME_BYTES {
            return Err(invalid("the frame image is too large"));
        }
        fs::read(path)
    }

    fn audio_exists(&self, session_id: &SessionId, relative_path: &str) -> bool {
        self.resolve(session_id, relative_path)
            .is_ok_and(|path| path.is_file())
    }

    fn delete_audio(&self, session_id: &SessionId, relative_path: &str) -> io::Result<u64> {
        // Only the audio folder: a corrupt row must never make retention delete a frame, the
        // export or anything else of the session.
        let parts = plain_components(relative_path)?;
        if parts[0] != AUDIO_DIR {
            return Err(invalid("only files in the audio folder can be deleted"));
        }
        let path = self.resolve(session_id, relative_path)?;
        let size = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => return Err(invalid("the path is not a regular file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(size),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn remove_session(&self, session_id: &SessionId) -> io::Result<()> {
        self.layout.remove_session_dir(session_id)
    }

    fn write_export(
        &self,
        session_id: &SessionId,
        file_name: &str,
        content: &str,
    ) -> io::Result<PathBuf> {
        if plain_components(file_name)?.len() != 1 {
            return Err(invalid("the export file name must not contain a folder"));
        }
        let dir = self.layout.session_dir(session_id)?;
        fs::create_dir_all(&dir)?;
        let target = dir.join(file_name);
        // Written next to the target and renamed, so a crash never leaves a half-written export.
        let temporary = dir.join(format!(".{file_name}.tmp"));
        fs::write(&temporary, content)?;
        fs::rename(&temporary, &target)?;
        Ok(target)
    }

    fn measure_usage(&self) -> io::Result<StorageUsage> {
        let mut usage = StorageUsage {
            total_bytes: directory_size(self.layout.root()),
            model_bytes: directory_size(&self.layout.root().join("models")),
            ..StorageUsage::default()
        };
        if let Ok(sessions) = fs::read_dir(self.layout.sessions_dir()) {
            for session in sessions.flatten() {
                usage.audio_bytes += directory_size(&session.path().join(AUDIO_DIR));
                usage.frame_bytes += directory_size(&session.path().join("frames"));
            }
        }
        Ok(usage)
    }
}

#[cfg(test)]
mod tests {
    use futures_executor::block_on;
    use rusqlite::Connection;
    use tan_application::ports::SessionRepository;
    use tan_domain::{AudioTrack, LanguageCode, LanguageSetting, Session, SessionKind};

    use super::*;
    use crate::db::{migrate_down_to, migrate_up, schema_version};
    use crate::repositories::SqliteSessionRepository;

    fn database_with_session(id: &str) -> Database {
        let db = Database::open_in_memory().unwrap();
        let session = Session::start_recording(
            SessionId::new(id),
            SessionKind::Meeting,
            "Weekly sync",
            EpochMs::new(1_000),
            LanguageSetting::Auto,
            LanguageCode::turkish(),
        );
        block_on(SqliteSessionRepository::new(db.clone()).create(&session)).unwrap();
        db
    }

    fn at(ms: i64) -> EpochMs {
        EpochMs::new(ms)
    }

    fn finished_job(id: &str) -> ProcessingJob {
        let mut job = ProcessingJob::new(SessionId::new(id), at(10));
        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        job.complete_step(ProcessingStep::Transcribe, at(12))
            .unwrap();
        job.start_step(ProcessingStep::DescribeFrames, at(13))
            .unwrap();
        job.skip_step(ProcessingStep::DescribeFrames, at(14))
            .unwrap();
        job.start_step(ProcessingStep::Synthesize, at(15)).unwrap();
        job.complete_step(ProcessingStep::Synthesize, at(16))
            .unwrap();
        job
    }

    // ----- jobs --------------------------------------------------------------------------

    #[test]
    fn a_job_round_trips_with_every_step_state() {
        let db = database_with_session("s1");
        let repo = SqliteProcessingRepository::new(db);
        assert_eq!(block_on(repo.get(&SessionId::new("s1"))).unwrap(), None);

        let job = finished_job("s1");
        block_on(repo.save(&job)).unwrap();
        let loaded = block_on(repo.get(&SessionId::new("s1"))).unwrap().unwrap();
        assert_eq!(loaded, job);
        assert_eq!(loaded.finished_at(), Some(at(16)));
        assert_eq!(
            *loaded.state(ProcessingStep::DescribeFrames),
            StepState::Skipped
        );
    }

    #[test]
    fn saving_again_replaces_the_job_and_keeps_failure_text() {
        let db = database_with_session("s1");
        let repo = SqliteProcessingRepository::new(db);
        block_on(repo.save(&finished_job("s1"))).unwrap();

        let mut failed = ProcessingJob::new(SessionId::new("s1"), at(20));
        failed
            .start_step(ProcessingStep::Transcribe, at(21))
            .unwrap();
        failed
            .fail_step(
                ProcessingStep::Transcribe,
                "the audio file is missing",
                at(22),
            )
            .unwrap();
        block_on(repo.save(&failed)).unwrap();

        let loaded = block_on(repo.get(&SessionId::new("s1"))).unwrap().unwrap();
        assert_eq!(loaded, failed);
        assert_eq!(
            loaded.failure(),
            Some((ProcessingStep::Transcribe, "the audio file is missing"))
        );
        assert_eq!(loaded.finished_at(), None);
    }

    #[test]
    fn a_job_needs_its_session_and_goes_when_the_session_goes() {
        let db = database_with_session("s1");
        let repo = SqliteProcessingRepository::new(db.clone());
        let orphan = block_on(repo.save(&finished_job("nobody"))).unwrap_err();
        assert!(matches!(orphan, RepositoryError::Conflict(_)), "{orphan}");

        block_on(repo.save(&finished_job("s1"))).unwrap();
        block_on(SqliteSessionRepository::new(db.clone()).delete(&SessionId::new("s1"))).unwrap();
        assert_eq!(block_on(repo.get(&SessionId::new("s1"))).unwrap(), None);
        let leftover: i64 = db
            .with_conn(|c| {
                Ok(c.query_row("SELECT count(*) FROM processing_steps", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(leftover, 0, "the steps are removed with the job");
    }

    #[test]
    fn a_job_with_a_missing_step_is_reported_as_corrupt() {
        let db = database_with_session("s1");
        let repo = SqliteProcessingRepository::new(db.clone());
        block_on(repo.save(&finished_job("s1"))).unwrap();
        db.with_conn(|c| {
            c.execute("DELETE FROM processing_steps WHERE step = 'synthesize'", [])?;
            Ok(())
        })
        .unwrap();
        let error = block_on(repo.get(&SessionId::new("s1"))).unwrap_err();
        assert!(matches!(error, RepositoryError::CorruptData(_)), "{error}");

        db.with_conn(|c| {
            c.execute("UPDATE processing_steps SET state = 'exploded'", [])?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            block_on(repo.get(&SessionId::new("s1"))).unwrap_err(),
            RepositoryError::CorruptData(_)
        ));
    }

    // ----- audio retention marker --------------------------------------------------------

    #[test]
    fn deleted_audio_is_marked_per_track_and_survives_a_track_rewrite() {
        let db = database_with_session("s1");
        let sessions = SqliteSessionRepository::new(db.clone());
        let repo = SqliteProcessingRepository::new(db);
        for kind in [TrackKind::Microphone, TrackKind::System] {
            block_on(sessions.save_audio_track(&AudioTrack {
                session_id: SessionId::new("s1"),
                kind,
                path: format!("audio/{kind}.wav"),
                sample_rate: 48_000,
                duration_ms: 5_000,
            }))
            .unwrap();
        }
        let id = SessionId::new("s1");
        assert_eq!(
            block_on(repo.deleted_tracks(&id)).unwrap(),
            Vec::<(TrackKind, EpochMs)>::new()
        );

        block_on(repo.mark_audio_deleted(&id, TrackKind::System, at(500))).unwrap();
        assert_eq!(
            block_on(repo.deleted_tracks(&id)).unwrap(),
            [(TrackKind::System, at(500))]
        );
        // The track row itself is still listed by the session repository.
        assert_eq!(block_on(sessions.list_audio_tracks(&id)).unwrap().len(), 2);

        let missing =
            block_on(repo.mark_audio_deleted(&SessionId::new("nope"), TrackKind::System, at(1)))
                .unwrap_err();
        assert!(matches!(missing, RepositoryError::NotFound { .. }));
    }

    // ----- migration ---------------------------------------------------------------------

    #[test]
    fn migration_three_keeps_existing_data_and_reverts_cleanly() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate_up(&mut conn).unwrap();
        migrate_down_to(&mut conn, 2).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 2);

        // An installed app at version 2, with a processed session and its audio.
        conn.execute_batch(
            "INSERT INTO sessions VALUES ('s1','meeting','Old','1000',NULL,'auto','tr','processed');
             INSERT INTO audio_tracks VALUES ('s1','system','audio/system.wav',48000,5000);",
        )
        .unwrap();
        migrate_up(&mut conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 3);
        let (title, deleted_at): (String, Option<i64>) = conn
            .query_row(
                "SELECT s.title, a.deleted_at FROM sessions s JOIN audio_tracks a ON a.session_id = s.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((title.as_str(), deleted_at), ("Old", None));
        let jobs: i64 = conn
            .query_row("SELECT count(*) FROM processing_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jobs, 0);

        migrate_down_to(&mut conn, 2).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 2);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'processing_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        let title: String = conn
            .query_row("SELECT title FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Old");
    }

    // ----- files -------------------------------------------------------------------------

    fn files() -> (tempfile::TempDir, DataDirFiles, DataLayout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path());
        let store = DataDirFiles::new(layout.clone());
        layout.ensure_session_dirs(&SessionId::new("s1")).unwrap();
        (dir, store, layout)
    }

    fn put(layout: &DataLayout, relative: &str, bytes: &[u8]) {
        let path = layout
            .session_dir(&SessionId::new("s1"))
            .unwrap()
            .join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn frames_are_read_from_the_session_folder_only() {
        let (_dir, store, layout) = files();
        put(&layout, "frames/a.png", b"png-bytes");
        let id = SessionId::new("s1");
        assert_eq!(store.read_frame(&id, "frames/a.png").unwrap(), b"png-bytes");
        assert!(store.read_frame(&id, "frames/missing.png").is_err());

        for escape in [
            "../s2/frames/a.png",
            "/etc/passwd",
            "frames/../../tan.db",
            "",
            ".",
        ] {
            let error = store.read_frame(&id, escape).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{escape:?}");
        }
        assert!(store
            .read_frame(&SessionId::new("../x"), "frames/a.png")
            .is_err());
    }

    #[test]
    fn audio_deletion_frees_bytes_is_repeatable_and_stays_in_the_audio_folder() {
        let (_dir, store, layout) = files();
        put(&layout, "audio/system.wav", &[0_u8; 1_234]);
        put(&layout, "frames/a.png", b"keep me");
        let id = SessionId::new("s1");

        assert!(store.audio_exists(&id, "audio/system.wav"));
        assert_eq!(store.delete_audio(&id, "audio/system.wav").unwrap(), 1_234);
        assert!(!store.audio_exists(&id, "audio/system.wav"));
        assert_eq!(
            store.delete_audio(&id, "audio/system.wav").unwrap(),
            0,
            "already gone"
        );

        // Anything but the audio folder is refused, so a bad row cannot delete a frame.
        for refused in ["frames/a.png", "notes.md", "../s2/audio/x.wav", "audio"] {
            assert!(store.delete_audio(&id, refused).is_err(), "{refused}");
        }
        assert_eq!(store.read_frame(&id, "frames/a.png").unwrap(), b"keep me");
    }

    #[test]
    fn the_export_is_written_into_the_session_folder_and_replaced_on_rewrite() {
        let (_dir, store, layout) = files();
        let id = SessionId::new("s1");
        let path = store.write_export(&id, "notes.md", "# first").unwrap();
        assert_eq!(path, layout.session_dir(&id).unwrap().join("notes.md"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "# first");
        store.write_export(&id, "notes.md", "# second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# second");
        let leftovers: Vec<_> = fs::read_dir(layout.session_dir(&id).unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        for bad in ["../notes.md", "sub/notes.md", "/abs.md", ""] {
            assert!(store.write_export(&id, bad, "x").is_err(), "{bad:?}");
        }
        // The folder is created when it is missing (an export never fails for that).
        assert!(store
            .write_export(&SessionId::new("fresh"), "notes.md", "x")
            .is_ok());
    }

    #[test]
    fn usage_is_split_into_audio_frames_models_and_the_total() {
        let (_dir, store, layout) = files();
        put(&layout, "audio/mic.wav", &[0_u8; 1_000]);
        put(&layout, "audio/system.wav", &[0_u8; 500]);
        put(&layout, "frames/a.png", &[0_u8; 300]);
        fs::create_dir_all(layout.root().join("models")).unwrap();
        fs::write(layout.root().join("models").join("ggml.bin"), [0_u8; 4_000]).unwrap();
        fs::write(layout.database_path(), [0_u8; 200]).unwrap();

        let usage = store.measure_usage().unwrap();
        assert_eq!(usage.audio_bytes, 1_500);
        assert_eq!(usage.frame_bytes, 300);
        assert_eq!(usage.model_bytes, 4_000);
        assert_eq!(usage.total_bytes, 1_500 + 300 + 4_000 + 200);
    }

    #[test]
    fn usage_of_an_empty_data_directory_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataDirFiles::new(DataLayout::new(dir.path().join("nothing-here")));
        assert_eq!(store.measure_usage().unwrap(), StorageUsage::default());
    }

    #[test]
    fn removing_a_session_deletes_its_folder_and_tolerates_a_missing_one() {
        let (_dir, store, layout) = files();
        put(&layout, "frames/a.png", b"x");
        let id = SessionId::new("s1");
        store.remove_session(&id).unwrap();
        assert!(!layout.session_dir(&id).unwrap().exists());
        store.remove_session(&id).unwrap();
    }
}

/// The whole pipeline over the real SQLite repositories and a real data directory; only the
/// AI models and the transcription engine are scripted.
#[cfg(test)]
mod pipeline_tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use futures_executor::block_on;
    use tan_application::ports::{
        AiError, AiResult, ChatModel, ChatRequest, ChatResponse, Clock, FrameRepository,
        IdGenerator, ModelCatalog, NoteRepository, ProviderFactory, ProviderProfileRepository,
        SearchIndex, SessionRepository, SettingsRepository, TaskAssignmentRepository,
        TranscriptRepository, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
        VisionModel, VisionRequest,
    };
    use tan_application::processing::{
        ProcessError, ProcessPorts, ProcessSession, ProcessSummary, SessionTranscriber,
    };
    use tan_application::sessions::{
        ApplyRetention, DeleteSession, ExportSessionMarkdown, GetSessionDetail, SessionDetail,
    };
    use tan_application::transcription::{
        TranscribeProgress, TranscribeResult, TranscribeSummary, TranscriptionEngine,
    };
    use tan_domain::{
        AudioTrack, Frame, FrameId, LanguageCode, LanguageSetting, ProviderKind, ProviderProfile,
        ProviderProfileId, Session, SessionKind, SessionStatus, SettingsPatch, TaskAssignment,
        TaskKind, TranscriptSegment,
    };

    use super::*;
    use crate::repositories::{
        SqliteFrameRepository, SqliteNoteRepository, SqliteProviderProfileRepository,
        SqliteSessionRepository, SqliteSettingsRepository, SqliteTaskAssignmentRepository,
        SqliteTranscriptRepository,
    };
    use crate::search::SqliteSearchIndex;

    const DAY_MS: i64 = 86_400_000;
    const STARTED_AT: i64 = 1_793_959_200_000;
    const DUE_2026_12_01: i64 = 1_796_083_200_000;

    const NOTE_JSON: &str = r#"{
      "summary": "Bütçe görüşüldü.",
      "decisions": ["Bütçe onaylandı"],
      "action_items": [{"task": "Raporu hazırla", "owner": "Ayşe", "due": "2026-12-01"}],
      "open_questions": ["Lansman tarihi nedir?"],
      "key_frames": [{"frame_id": "F2", "reason": "fiyat tablosu"}]
    }"#;
    const VISION: &str = r#"{"description": "Fiyat tablosu", "visible_text": "Plan A 10 TL"}"#;

    #[derive(Default)]
    struct Script {
        chat: Mutex<VecDeque<String>>,
        vision: Mutex<VecDeque<String>>,
        transcribe_calls: AtomicUsize,
    }

    fn answer(next: Option<String>) -> AiResult<ChatResponse> {
        next.map(|text| ChatResponse {
            text,
            usage: None,
            truncated: false,
        })
        .ok_or(AiError::Timeout)
    }

    #[derive(Clone)]
    struct Ai(Arc<Script>);

    impl ChatModel for Ai {
        async fn complete(&self, _request: ChatRequest) -> AiResult<ChatResponse> {
            let next = self.0.chat.lock().unwrap().pop_front();
            answer(next)
        }
    }

    impl VisionModel for Ai {
        async fn describe(&self, _request: VisionRequest) -> AiResult<ChatResponse> {
            let next = self.0.vision.lock().unwrap().pop_front();
            answer(next)
        }
    }

    impl TranscriptionModel for Ai {
        async fn transcribe(
            &self,
            _request: TranscriptionRequest,
        ) -> AiResult<Vec<TranscriptSegmentDto>> {
            Err(AiError::Unsupported("not used".to_owned()))
        }
    }

    impl ModelCatalog for Ai {
        async fn list_models(&self) -> AiResult<Vec<String>> {
            Err(AiError::Unsupported("not used".to_owned()))
        }
    }

    struct Factory(Ai);

    impl ProviderFactory for Factory {
        type Chat = Ai;
        type Vision = Ai;
        type Transcription = Ai;
        type Catalog = Ai;

        async fn chat(&self, _: &ProviderProfile) -> AiResult<Ai> {
            Ok(self.0.clone())
        }

        async fn vision(&self, _: &ProviderProfile) -> AiResult<Ai> {
            Ok(self.0.clone())
        }

        async fn transcription(&self, _: &ProviderProfile) -> AiResult<Ai> {
            Ok(self.0.clone())
        }

        async fn catalog(&self, _: &ProviderProfile) -> AiResult<Ai> {
            Ok(self.0.clone())
        }
    }

    /// Stores two segments, like a finished transcription would.
    struct Transcriber {
        script: Arc<Script>,
        transcripts: SqliteTranscriptRepository,
    }

    impl SessionTranscriber for Transcriber {
        async fn transcribe(
            &self,
            session_id: &SessionId,
            _on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
        ) -> TranscribeResult<TranscribeSummary> {
            self.script.transcribe_calls.fetch_add(1, Ordering::SeqCst);
            let segment = |track, start_ms, speaker: &str, text: &str| TranscriptSegment {
                session_id: session_id.clone(),
                track,
                start_ms,
                end_ms: start_ms + 2_000,
                language: Some(LanguageCode::turkish()),
                text: text.to_owned(),
                speaker: Some(speaker.to_owned()),
            };
            let segments = [
                segment(TrackKind::Microphone, 0, "me", "Merhaba, başlayalım."),
                segment(TrackKind::System, 5_000, "others", "Bütçeyi onaylıyoruz."),
            ];
            self.transcripts
                .replace_for_session(session_id, &segments)
                .await
                .unwrap();
            Ok(TranscribeSummary {
                session_id: session_id.clone(),
                segment_count: segments.len(),
                engine: TranscriptionEngine::LocalWhisper,
            })
        }
    }

    #[derive(Default)]
    struct Ids(AtomicU64);

    impl IdGenerator for Ids {
        fn new_id(&self) -> String {
            format!("id-{}", self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now(&self) -> EpochMs {
            EpochMs::new(self.0.load(Ordering::SeqCst))
        }
    }

    type Subject = ProcessSession<
        SqliteSessionRepository,
        SqliteFrameRepository,
        SqliteTranscriptRepository,
        SqliteNoteRepository,
        SqliteTaskAssignmentRepository,
        SqliteProviderProfileRepository,
        Factory,
        SqliteProcessingRepository,
        DataDirFiles,
        Transcriber,
    >;

    struct Rig {
        _dir: tempfile::TempDir,
        layout: DataLayout,
        db: Database,
        script: Arc<Script>,
        clock: Arc<TestClock>,
        files: Arc<DataDirFiles>,
        process: Subject,
    }

    fn sid() -> SessionId {
        SessionId::new("s1")
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path());
        let db = Database::open(&layout.database_path()).unwrap();
        let script = Arc::new(Script::default());
        let clock = Arc::new(TestClock(AtomicI64::new(STARTED_AT + 3_600_000)));
        let files = Arc::new(DataDirFiles::new(layout.clone()));

        // A recorded session with two audio files, two frame images and two profiles.
        let sessions = Arc::new(SqliteSessionRepository::new(db.clone()));
        let frames = Arc::new(SqliteFrameRepository::new(db.clone()));
        let transcripts = Arc::new(SqliteTranscriptRepository::new(db.clone()));
        let profiles = Arc::new(SqliteProviderProfileRepository::new(db.clone()));
        let assignments = Arc::new(SqliteTaskAssignmentRepository::new(db.clone()));
        let mut session = Session::start_recording(
            sid(),
            SessionKind::Meeting,
            "Bütçe toplantısı",
            EpochMs::new(STARTED_AT),
            LanguageSetting::Fixed(LanguageCode::turkish()),
            LanguageCode::turkish(),
        );
        session
            .finish_recording(EpochMs::new(STARTED_AT + 1_800_000))
            .unwrap();
        block_on(sessions.create(&session)).unwrap();
        layout.ensure_session_dirs(&sid()).unwrap();
        let session_dir = layout.session_dir(&sid()).unwrap();
        for kind in [TrackKind::Microphone, TrackKind::System] {
            let path = format!("audio/{kind}.wav");
            fs::write(session_dir.join(&path), vec![0_u8; 2_000]).unwrap();
            block_on(sessions.save_audio_track(&AudioTrack {
                session_id: sid(),
                kind,
                path,
                sample_rate: 48_000,
                duration_ms: 1_800_000,
            }))
            .unwrap();
        }
        for (id, timestamp_ms) in [("f1", 3_000_u64), ("f2", 6_000)] {
            let path = format!("frames/{id}.png");
            fs::write(session_dir.join(&path), b"\x89PNG fake").unwrap();
            block_on(frames.add(&Frame {
                id: FrameId::new(id),
                session_id: sid(),
                timestamp_ms,
                path,
                perceptual_hash: u64::MAX - timestamp_ms,
                description: None,
            }))
            .unwrap();
        }
        for (id, kind, task) in [
            ("chat", ProviderKind::OpenAi, TaskKind::Summary),
            ("vision", ProviderKind::Gemini, TaskKind::Vision),
        ] {
            block_on(profiles.upsert(&ProviderProfile {
                id: ProviderProfileId::new(id),
                kind,
                display_name: id.to_owned(),
                model: "m".to_owned(),
                base_url: None,
                secret_ref: None,
                supports_vision: false,
            }))
            .unwrap();
            block_on(assignments.set(&TaskAssignment {
                task,
                profile_id: ProviderProfileId::new(id),
            }))
            .unwrap();
        }

        let process = ProcessSession::new(ProcessPorts {
            sessions,
            frames,
            transcripts: transcripts.clone(),
            notes: Arc::new(SqliteNoteRepository::new(db.clone())),
            assignments,
            profiles,
            factory: Arc::new(Factory(Ai(script.clone()))),
            jobs: Arc::new(SqliteProcessingRepository::new(db.clone())),
            files: files.clone(),
            transcriber: Arc::new(Transcriber {
                script: script.clone(),
                transcripts: (*transcripts).clone(),
            }),
            ids: Arc::new(Ids::default()),
            clock: clock.clone(),
        });
        Rig {
            _dir: dir,
            layout,
            db,
            script,
            clock,
            files,
            process,
        }
    }

    impl Rig {
        fn script(&self, chat: &[&str], vision: &[&str]) {
            self.script
                .chat
                .lock()
                .unwrap()
                .extend(chat.iter().map(|s| (*s).to_owned()));
            self.script
                .vision
                .lock()
                .unwrap()
                .extend(vision.iter().map(|s| (*s).to_owned()));
        }

        fn run(&self) -> Result<ProcessSummary, ProcessError> {
            block_on(self.process.execute(&sid(), &|_| {}))
        }

        fn transcribe_calls(&self) -> usize {
            self.script.transcribe_calls.load(Ordering::SeqCst)
        }

        fn status(&self) -> SessionStatus {
            block_on(SqliteSessionRepository::new(self.db.clone()).get(&sid()))
                .unwrap()
                .unwrap()
                .status()
        }

        fn detail(&self) -> SessionDetail {
            let db = &self.db;
            let repo = Arc::new(SqliteProcessingRepository::new(db.clone()));
            block_on(
                GetSessionDetail::new(
                    Arc::new(SqliteSessionRepository::new(db.clone())),
                    Arc::new(SqliteFrameRepository::new(db.clone())),
                    Arc::new(SqliteTranscriptRepository::new(db.clone())),
                    Arc::new(SqliteNoteRepository::new(db.clone())),
                    repo.clone(),
                    repo,
                )
                .execute(&sid()),
            )
            .unwrap()
        }
    }

    #[test]
    fn a_session_is_processed_into_a_stored_searchable_note_with_real_repositories() {
        let rig = rig();
        rig.script(&[NOTE_JSON], &[VISION, VISION]);
        let summary = rig.run().unwrap();
        assert_eq!(rig.status(), SessionStatus::Processed);
        assert_eq!(summary.action_items, 1);

        let detail = rig.detail();
        let note = detail.note.expect("a note is stored");
        assert_eq!(note.note.summary, "Bütçe görüşüldü.");
        assert_eq!(note.note.decisions, ["Bütçe onaylandı"]);
        assert_eq!(note.note.open_questions, ["Lansman tarihi nedir?"]);
        assert_eq!(note.note.frame_refs, [FrameId::new("f2")]);
        assert_eq!(note.action_items[0].task, "Raporu hazırla");
        assert_eq!(note.action_items[0].owner.as_deref(), Some("Ayşe"));
        assert_eq!(note.action_items[0].due, Some(EpochMs::new(DUE_2026_12_01)));
        assert!(!note.action_items[0].done);
        assert!(detail.frames.iter().all(|f| f
            .description
            .as_deref()
            .is_some_and(|d| d.contains("Fiyat tablosu"))));
        assert_eq!(detail.transcript.len(), 2);
        let job = detail.job.expect("the job is stored");
        assert!(job.is_complete());
        assert_eq!(*job.state(ProcessingStep::Synthesize), StepState::Done);

        // The note is in the full-text index (Turkish-aware), next to the transcript.
        let index = SqliteSearchIndex::new(rig.db.clone());
        let hits = block_on(index.search("BÜTÇE", 10)).unwrap();
        assert!(hits.iter().any(|h| h.session_id == sid()), "{hits:?}");
    }

    #[test]
    fn a_failed_run_is_stored_as_failed_and_retry_resumes_from_the_database() {
        let rig = rig();
        // The chat model "times out" (nothing scripted); vision works.
        rig.script(&[], &[VISION, VISION]);
        let error = rig.run().unwrap_err();
        assert!(matches!(error, ProcessError::Provider(AiError::Timeout)));
        assert_eq!(rig.status(), SessionStatus::Failed);
        let detail = rig.detail();
        let job = detail.job.unwrap();
        assert_eq!(
            job.failure().map(|(step, _)| step),
            Some(ProcessingStep::Synthesize)
        );
        assert!(detail.note.is_none());

        // Retrying (the database holds the job) does not repeat the earlier work.
        rig.script(&[NOTE_JSON], &[]);
        let calls_before = rig.transcribe_calls();
        block_on(rig.process.retry(&sid(), &|_| {})).unwrap();
        assert_eq!(rig.status(), SessionStatus::Processed);
        assert_eq!(rig.transcribe_calls(), calls_before);
        assert!(rig.detail().note.is_some());
    }

    #[test]
    fn processing_again_leaves_a_single_note_and_a_single_job() {
        let rig = rig();
        rig.script(&[NOTE_JSON, NOTE_JSON], &[VISION, VISION, VISION, VISION]);
        let first = rig.run().unwrap();
        rig.clock.0.fetch_add(1_000, Ordering::SeqCst);
        let second = rig.run().unwrap();
        assert_ne!(first.note_id, second.note_id);

        let counts: (i64, i64, i64) = rig
            .db
            .with_conn(|c| {
                Ok((
                    c.query_row("SELECT count(*) FROM notes", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM processing_jobs", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM action_items", [], |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!(counts, (1, 1, 1));
        let notes = SqliteNoteRepository::new(rig.db.clone());
        assert!(block_on(notes.get(&first.note_id)).unwrap().is_none());
        assert!(block_on(notes.get(&second.note_id)).unwrap().is_some());
    }

    #[test]
    fn retention_deletes_only_audio_files_and_the_session_can_still_be_reprocessed() {
        let rig = rig();
        rig.script(&[NOTE_JSON, NOTE_JSON], &[VISION, VISION, VISION, VISION]);
        rig.run().unwrap();

        let settings = Arc::new(SqliteSettingsRepository::new(rig.db.clone()));
        block_on(settings.update(&SettingsPatch {
            audio_retention_days: Some(Some(7)),
            ..SettingsPatch::default()
        }))
        .unwrap();
        let processing = Arc::new(SqliteProcessingRepository::new(rig.db.clone()));
        let retention = ApplyRetention::new(
            Arc::new(SqliteSessionRepository::new(rig.db.clone())),
            processing.clone(),
            processing,
            rig.files.clone(),
            settings,
            rig.clock.clone(),
        );

        // Not old enough yet.
        assert_eq!(block_on(retention.execute()).unwrap().files_deleted, 0);

        rig.clock.0.fetch_add(8 * DAY_MS, Ordering::SeqCst);
        let report = block_on(retention.execute()).unwrap();
        assert_eq!((report.sessions_cleaned, report.files_deleted), (1, 2));
        assert_eq!(report.bytes_freed, 4_000);

        let dir = rig.layout.session_dir(&sid()).unwrap();
        assert!(!dir.join("audio/microphone.wav").exists());
        assert!(!dir.join("audio/system.wav").exists());
        assert!(dir.join("frames/f1.png").exists(), "frames stay");
        let detail = rig.detail();
        assert!(detail.note.is_some());
        assert_eq!((detail.frames.len(), detail.transcript.len()), (2, 2));
        assert_eq!(detail.audio_tracks.len(), 2);
        assert!(detail.audio_tracks.iter().all(|t| t.deleted_at.is_some()));
        assert_eq!(block_on(retention.execute()).unwrap().files_deleted, 0);

        // Processing again works from the stored transcript: no audio is needed.
        let calls_before = rig.transcribe_calls();
        rig.run().unwrap();
        assert_eq!(rig.transcribe_calls(), calls_before);
    }

    #[test]
    fn the_export_and_the_deletion_work_on_the_real_data_directory() {
        let rig = rig();
        rig.script(&[NOTE_JSON], &[VISION, VISION]);
        rig.run().unwrap();

        let export = ExportSessionMarkdown::new(rig.files.clone());
        let path = export.execute(&rig.detail()).unwrap();
        assert_eq!(
            path,
            rig.layout.session_dir(&sid()).unwrap().join("notes.md")
        );
        let markdown = fs::read_to_string(&path).unwrap();
        assert!(markdown.contains("- [ ] Raporu hazırla (sorumlu: Ayşe, tarih: 2026-12-01)"));
        // The image link resolves from the export's own folder.
        assert!(markdown.contains("](frames/f2.png)"));
        assert!(path.parent().unwrap().join("frames/f2.png").is_file());

        let delete = DeleteSession::new(
            Arc::new(SqliteSessionRepository::new(rig.db.clone())),
            rig.files.clone(),
        );
        block_on(delete.execute(&sid())).unwrap();
        assert!(!rig.layout.session_dir(&sid()).unwrap().exists());
        let leftovers: i64 = rig
            .db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT (SELECT count(*) FROM sessions) + (SELECT count(*) FROM notes)
                     + (SELECT count(*) FROM frames) + (SELECT count(*) FROM action_items)
                     + (SELECT count(*) FROM processing_jobs) + (SELECT count(*) FROM search_docs)
                     + (SELECT count(*) FROM transcript_segments)
                     + (SELECT count(*) FROM audio_tracks)",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(leftovers, 0, "every row of the session is gone");
    }
}
