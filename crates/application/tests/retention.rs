//! Audio retention, session deletion and the storage report against in-memory fakes.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]
// `assert!(x.is_empty())` reads fine in tests.
#![allow(clippy::assert_is_empty)]

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use tan_application::ports::{Clock, SessionRepository, SettingsRepository};
use tan_application::processing::ProcessingJobRepository;
use tan_application::sessions::{
    ApplyRetention, AudioRetentionRepository, DeleteSession, GetStorageUsage, SessionFileStore,
    SessionsError, StorageUsage,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    AppSettings, AudioTrack, EpochMs, LanguageCode, LanguageSetting, ProcessingJob, ProcessingStep,
    Session, SessionId, SessionKind, SessionRecord, SessionStatus, SettingsPatch, TrackKind,
};

const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct World {
    sessions: Mutex<Vec<Session>>,
    tracks: Mutex<Vec<AudioTrack>>,
    jobs: Mutex<Vec<ProcessingJob>>,
    deleted: Mutex<Vec<(String, TrackKind, EpochMs)>>,
    settings: Mutex<AppSettings>,
    // What the file store was asked to do, in order.
    file_log: Mutex<Vec<String>>,
    // When set, `list` returns this snapshot instead of the current sessions.
    stale_list: Mutex<Option<Vec<Session>>>,
    // Audio paths of this session fail to delete.
    failing_session: Mutex<Option<String>>,
    now: AtomicI64,
}

impl SessionRepository for World {
    async fn create(&self, session: &Session) -> RepositoryResult<()> {
        self.sessions.lock().unwrap().push(session.clone());
        Ok(())
    }

    async fn update(&self, _session: &Session) -> RepositoryResult<()> {
        Ok(())
    }

    async fn get(&self, id: &SessionId) -> RepositoryResult<Option<Session>> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| &s.id == id)
            .cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<Session>> {
        if let Some(snapshot) = self.stale_list.lock().unwrap().clone() {
            return Ok(snapshot);
        }
        Ok(self.sessions.lock().unwrap().clone())
    }

    async fn delete(&self, id: &SessionId) -> RepositoryResult<()> {
        self.file_log
            .lock()
            .unwrap()
            .push(format!("db-delete {id}"));
        self.sessions.lock().unwrap().retain(|s| &s.id != id);
        Ok(())
    }

    async fn save_audio_track(&self, track: &AudioTrack) -> RepositoryResult<()> {
        self.tracks.lock().unwrap().push(track.clone());
        Ok(())
    }

    async fn list_audio_tracks(&self, session_id: &SessionId) -> RepositoryResult<Vec<AudioTrack>> {
        Ok(self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| &t.session_id == session_id)
            .cloned()
            .collect())
    }
}

impl ProcessingJobRepository for World {
    async fn get(&self, session_id: &SessionId) -> Result<Option<ProcessingJob>, RepositoryError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.session_id() == session_id)
            .cloned())
    }

    async fn save(&self, job: &ProcessingJob) -> Result<(), RepositoryError> {
        self.jobs.lock().unwrap().push(job.clone());
        Ok(())
    }
}

impl AudioRetentionRepository for World {
    async fn deleted_tracks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<(TrackKind, EpochMs)>, RepositoryError> {
        Ok(self
            .deleted
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _, _)| id == session_id.as_str())
            .map(|(_, kind, at)| (*kind, *at))
            .collect())
    }

    async fn mark_audio_deleted(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        at: EpochMs,
    ) -> Result<(), RepositoryError> {
        self.deleted
            .lock()
            .unwrap()
            .push((session_id.to_string(), kind, at));
        Ok(())
    }
}

impl SettingsRepository for World {
    async fn load(&self) -> RepositoryResult<AppSettings> {
        Ok(self.settings.lock().unwrap().clone())
    }

    async fn update(&self, _patch: &SettingsPatch) -> RepositoryResult<AppSettings> {
        Ok(self.settings.lock().unwrap().clone())
    }
}

impl SessionFileStore for World {
    fn read_frame(&self, _: &SessionId, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other("not used"))
    }

    fn audio_exists(&self, _: &SessionId, _: &str) -> bool {
        true
    }

    fn delete_audio(&self, session_id: &SessionId, relative_path: &str) -> io::Result<u64> {
        if self.failing_session.lock().unwrap().as_deref() == Some(session_id.as_str()) {
            return Err(io::Error::other("the file is locked"));
        }
        self.file_log
            .lock()
            .unwrap()
            .push(format!("delete-audio {session_id} {relative_path}"));
        Ok(1_000)
    }

    fn remove_session(&self, session_id: &SessionId) -> io::Result<()> {
        self.file_log
            .lock()
            .unwrap()
            .push(format!("remove-session {session_id}"));
        Ok(())
    }

    fn write_export(&self, _: &SessionId, _: &str, _: &str) -> io::Result<PathBuf> {
        Err(io::Error::other("not used"))
    }

    fn measure_usage(&self) -> io::Result<StorageUsage> {
        Ok(StorageUsage {
            total_bytes: 100,
            audio_bytes: 60,
            frame_bytes: 30,
            model_bytes: 5,
        })
    }
}

impl Clock for World {
    fn now(&self) -> EpochMs {
        EpochMs::new(self.now.load(Ordering::SeqCst))
    }
}

type Retention = ApplyRetention<World, World, World, World, World>;

fn world(retention_days: Option<u32>) -> Arc<World> {
    let world = World::default();
    world.now.store(NOW, Ordering::SeqCst);
    world.settings.lock().unwrap().audio_retention_days = retention_days;
    Arc::new(world)
}

fn subject(world: &Arc<World>) -> Retention {
    ApplyRetention::new(
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
    )
}

/// Adds a session with both audio tracks; `finished_ago_ms` is when its job finished
/// (`None`: no job).
fn add_session(world: &World, id: &str, status: SessionStatus, finished_ago_ms: Option<i64>) {
    let session = Session::restore(SessionRecord {
        id: SessionId::new(id),
        kind: SessionKind::Meeting,
        title: id.to_owned(),
        started_at: EpochMs::new(1),
        ended_at: None,
        language: LanguageSetting::Auto,
        note_language: LanguageCode::turkish(),
        status,
    })
    .unwrap();
    world.sessions.lock().unwrap().push(session);
    for kind in [TrackKind::Microphone, TrackKind::System] {
        world.tracks.lock().unwrap().push(AudioTrack {
            session_id: SessionId::new(id),
            kind,
            path: format!("audio/{kind}.wav"),
            sample_rate: 48_000,
            duration_ms: 1_000,
        });
    }
    if let Some(ago_ms) = finished_ago_ms {
        let finished_at = NOW - ago_ms;
        let at = EpochMs::new(finished_at);
        let mut job = ProcessingJob::new(SessionId::new(id), at);
        for step in [
            ProcessingStep::Transcribe,
            ProcessingStep::DescribeFrames,
            ProcessingStep::Synthesize,
        ] {
            job.start_step(step, at).unwrap();
            job.complete_step(step, at).unwrap();
        }
        assert_eq!(job.finished_at(), Some(at));
        world.jobs.lock().unwrap().push(job);
    }
}

fn deleted_audio(world: &World) -> Vec<String> {
    world
        .file_log
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| entry.starts_with("delete-audio"))
        .cloned()
        .collect()
}

#[test]
fn audio_of_expired_processed_sessions_is_deleted_and_its_tracks_are_marked() {
    let world = world(Some(30));
    add_session(&world, "old", SessionStatus::Processed, Some(31 * DAY_MS));

    let report = block_on(subject(&world).execute()).unwrap();
    assert_eq!(report.sessions_cleaned, 1);
    assert_eq!(report.files_deleted, 2);
    assert_eq!(report.bytes_freed, 2_000);
    assert_eq!(report.sessions_failed, 0);
    assert_eq!(
        deleted_audio(&world),
        [
            "delete-audio old audio/microphone.wav",
            "delete-audio old audio/system.wav"
        ]
    );

    // Both track rows are kept and marked with the deletion time; the rest of the session
    // (everything but the audio) is untouched.
    let tracks = block_on(SessionRepository::list_audio_tracks(
        &*world,
        &SessionId::new("old"),
    ))
    .unwrap();
    assert_eq!(tracks.len(), 2);
    let marked = block_on(world.deleted_tracks(&SessionId::new("old"))).unwrap();
    assert_eq!(marked.len(), 2);
    assert!(marked.iter().all(|(_, at)| at.get() == NOW));
    assert_eq!(world.sessions.lock().unwrap().len(), 1);
    assert_eq!(
        world.file_log.lock().unwrap().len(),
        2,
        "only audio files were touched: no session folder removal, no frames"
    );
}

#[test]
fn nothing_is_deleted_without_a_retention_setting() {
    let world = world(None);
    add_session(
        &world,
        "old",
        SessionStatus::Processed,
        Some(10000 * DAY_MS),
    );
    let report = block_on(subject(&world).execute()).unwrap();
    assert_eq!(report.files_deleted, 0);
    assert!(world.file_log.lock().unwrap().is_empty());
    assert!(world.deleted.lock().unwrap().is_empty());
}

#[test]
fn only_processed_sessions_that_are_old_enough_are_touched() {
    let world = world(Some(30));
    add_session(
        &world,
        "processing",
        SessionStatus::Processing,
        Some(90 * DAY_MS),
    );
    add_session(
        &world,
        "recorded",
        SessionStatus::Recorded,
        Some(90 * DAY_MS),
    );
    add_session(&world, "failed", SessionStatus::Failed, Some(90 * DAY_MS));
    add_session(&world, "recording", SessionStatus::Recording, None);
    add_session(
        &world,
        "recent",
        SessionStatus::Processed,
        Some(29 * DAY_MS + DAY_MS / 2),
    );
    add_session(&world, "no-job", SessionStatus::Processed, None);
    add_session(&world, "old", SessionStatus::Processed, Some(30 * DAY_MS)); // exactly N days

    let report = block_on(subject(&world).execute()).unwrap();
    assert_eq!(report.sessions_cleaned, 1);
    assert_eq!(
        deleted_audio(&world),
        [
            "delete-audio old audio/microphone.wav",
            "delete-audio old audio/system.wav"
        ]
    );
}

#[test]
fn a_session_whose_processing_started_after_the_list_was_read_is_left_alone() {
    let world = world(Some(30));
    add_session(&world, "raced", SessionStatus::Processed, Some(31 * DAY_MS));
    // The retention run read this snapshot, then the user started processing the session.
    let snapshot = world.sessions.lock().unwrap().clone();
    *world.stale_list.lock().unwrap() = Some(snapshot);
    let current = Session::restore(SessionRecord {
        id: SessionId::new("raced"),
        kind: SessionKind::Meeting,
        title: "raced".to_owned(),
        started_at: EpochMs::new(1),
        ended_at: None,
        language: LanguageSetting::Auto,
        note_language: LanguageCode::turkish(),
        status: SessionStatus::Processing,
    })
    .unwrap();
    *world.sessions.lock().unwrap() = vec![current];

    let report = block_on(subject(&world).execute()).unwrap();
    assert_eq!(report.files_deleted, 0);
    assert!(world.file_log.lock().unwrap().is_empty());
    assert!(world.deleted.lock().unwrap().is_empty());
}

#[test]
fn running_it_again_does_not_repeat_the_work() {
    let world = world(Some(7));
    add_session(&world, "old", SessionStatus::Processed, Some(8 * DAY_MS));
    let retention = subject(&world);
    block_on(retention.execute()).unwrap();
    let second = block_on(retention.execute()).unwrap();
    assert_eq!(second.files_deleted, 0);
    assert_eq!(deleted_audio(&world).len(), 2);
}

#[test]
fn one_failing_session_does_not_stop_the_others_and_is_retried_next_time() {
    let world = world(Some(7));
    add_session(&world, "locked", SessionStatus::Processed, Some(8 * DAY_MS));
    add_session(&world, "fine", SessionStatus::Processed, Some(9 * DAY_MS));
    *world.failing_session.lock().unwrap() = Some("locked".to_owned());
    let retention = subject(&world);

    let report = block_on(retention.execute()).unwrap();
    assert_eq!((report.sessions_cleaned, report.sessions_failed), (1, 1));
    assert!(deleted_audio(&world).iter().all(|e| e.contains("fine")));

    *world.failing_session.lock().unwrap() = None;
    let report = block_on(retention.execute()).unwrap();
    assert_eq!((report.sessions_cleaned, report.sessions_failed), (1, 0));
    assert!(deleted_audio(&world).iter().any(|e| e.contains("locked")));
}

#[test]
fn deleting_a_session_removes_its_files_and_then_its_rows() {
    let world = world(None);
    add_session(&world, "done", SessionStatus::Processed, Some(DAY_MS));
    let delete = DeleteSession::new(world.clone(), world.clone());

    block_on(delete.execute(&SessionId::new("done"))).unwrap();
    assert!(world.sessions.lock().unwrap().is_empty());
    assert_eq!(
        *world.file_log.lock().unwrap(),
        ["remove-session done", "db-delete done"],
        "files first, so a failed file removal leaves a session that can be deleted again"
    );
}

#[test]
fn a_session_that_is_recording_or_processing_cannot_be_deleted() {
    let world = world(None);
    add_session(&world, "rec", SessionStatus::Recording, None);
    add_session(&world, "busy", SessionStatus::Processing, None);
    let delete = DeleteSession::new(world.clone(), world.clone());

    for id in ["rec", "busy"] {
        let error = block_on(delete.execute(&SessionId::new(id))).unwrap_err();
        assert!(matches!(error, SessionsError::Busy(_)), "{error}");
    }
    assert_eq!(world.sessions.lock().unwrap().len(), 2);
    assert!(world.file_log.lock().unwrap().is_empty());

    let missing = block_on(delete.execute(&SessionId::new("nope"))).unwrap_err();
    assert!(matches!(missing, SessionsError::NotFound("session")));
}

#[test]
fn the_storage_report_comes_from_the_file_store() {
    let world = world(None);
    let usage = GetStorageUsage::new(world).execute().unwrap();
    assert_eq!(
        usage,
        StorageUsage {
            total_bytes: 100,
            audio_bytes: 60,
            frame_bytes: 30,
            model_bytes: 5,
        }
    );
}
