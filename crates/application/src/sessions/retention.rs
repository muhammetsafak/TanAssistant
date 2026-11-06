//! Disk-space policy: "delete audio N days after processing" and the usage report.

use std::sync::Arc;

use tan_domain::{SessionId, SessionStatus, TrackKind};

use super::{
    AudioRetentionRepository, SessionFileStore, SessionsError, SessionsResult, StorageUsage,
};
use crate::ports::{Clock, SessionRepository, SettingsRepository};
use crate::processing::ProcessingJobRepository;

const MS_PER_DAY: i64 = 86_400_000;

/// What a retention run removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionReport {
    /// Sessions whose audio was (partly) deleted by this run.
    pub sessions_cleaned: usize,
    /// Sessions that could not be cleaned (the next run tries again).
    pub sessions_failed: usize,
    /// Audio files deleted.
    pub files_deleted: usize,
    /// Bytes freed.
    pub bytes_freed: u64,
}

/// Deletes the **audio files** of sessions that were processed more than
/// `AppSettings::audio_retention_days` days ago. The note, the transcript and the frames stay,
/// and each audio track keeps its row, marked as deleted.
///
/// Only `Processed` sessions are touched (never one that is recording or being processed), and
/// the age counts from when the processing job finished. With no retention setting nothing is
/// deleted. The run is idempotent: a file that is already gone is just marked.
pub struct ApplyRetention<S, J, R, W, T> {
    sessions: Arc<S>,
    jobs: Arc<J>,
    retention: Arc<R>,
    files: Arc<W>,
    settings: Arc<T>,
    clock: Arc<dyn Clock>,
}

impl<S, J, R, W, T> ApplyRetention<S, J, R, W, T>
where
    S: SessionRepository,
    J: ProcessingJobRepository,
    R: AudioRetentionRepository,
    W: SessionFileStore,
    T: SettingsRepository,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(
        sessions: Arc<S>,
        jobs: Arc<J>,
        retention: Arc<R>,
        files: Arc<W>,
        settings: Arc<T>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            sessions,
            jobs,
            retention,
            files,
            settings,
            clock,
        }
    }

    /// Runs the policy once.
    pub async fn execute(&self) -> SessionsResult<RetentionReport> {
        let mut report = RetentionReport::default();
        let Some(days) = self.settings.load().await?.audio_retention_days else {
            return Ok(report);
        };
        let cutoff = self
            .clock
            .now()
            .get()
            .saturating_sub(i64::from(days).saturating_mul(MS_PER_DAY));

        for session in self.sessions.list().await? {
            if session.status() != SessionStatus::Processed {
                continue;
            }
            let Some(finished_at) = self
                .jobs
                .get(&session.id)
                .await?
                .and_then(|job| job.finished_at())
            else {
                continue; // processed before jobs were recorded: its age is unknown
            };
            if finished_at.get() > cutoff {
                continue;
            }
            // The list above is a snapshot and cleaning many sessions takes a while: a session
            // the user started processing meanwhile has to be left alone, so look at its state
            // again right before touching its files.
            let still_processed = self
                .sessions
                .get(&session.id)
                .await?
                .is_some_and(|current| current.status() == SessionStatus::Processed);
            if !still_processed {
                continue;
            }
            // One session's failure (a locked file, say) must not keep the others' audio.
            match self.clean_session(&session.id).await {
                Ok((0, _)) => {}
                Ok((files, bytes)) => {
                    report.sessions_cleaned += 1;
                    report.files_deleted += files;
                    report.bytes_freed += bytes;
                }
                Err(_) => report.sessions_failed += 1,
            }
        }
        Ok(report)
    }

    /// Deletes the not-yet-deleted audio files of one session; returns files and bytes.
    async fn clean_session(&self, id: &SessionId) -> SessionsResult<(usize, u64)> {
        let already_deleted: Vec<TrackKind> = self
            .retention
            .deleted_tracks(id)
            .await?
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        let (mut files, mut bytes) = (0, 0);
        for track in self.sessions.list_audio_tracks(id).await? {
            if already_deleted.contains(&track.kind) {
                continue;
            }
            bytes += self.files.delete_audio(id, &track.path)?;
            self.retention
                .mark_audio_deleted(id, track.kind, self.clock.now())
                .await?;
            files += 1;
        }
        Ok((files, bytes))
    }
}

/// Reports how much disk the application's data uses.
pub struct GetStorageUsage<W> {
    files: Arc<W>,
}

impl<W: SessionFileStore> GetStorageUsage<W> {
    /// Creates the use case.
    #[must_use]
    pub fn new(files: Arc<W>) -> Self {
        Self { files }
    }

    /// Measures the data directory.
    pub fn execute(&self) -> SessionsResult<StorageUsage> {
        self.files.measure_usage().map_err(SessionsError::from)
    }
}
