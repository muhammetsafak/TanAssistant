//! The `Session` aggregate root and its lifecycle rules.

use std::error::Error;
use std::fmt;

use crate::ids::SessionId;
use crate::language::{LanguageCode, LanguageSetting};
use crate::EpochMs;

string_enum! {
    /// What produced a session. Source-specific data never lives on [`Session`] itself.
    ///
    /// `ImportedFile` (file import, a later plan) is added here when that work starts.
    #[non_exhaustive]
    pub enum SessionKind {
        /// A live meeting/conversation recorded from the machine's audio and screen.
        Meeting => "meeting",
    }
}

string_enum! {
    /// Where a session is in its lifecycle.
    pub enum SessionStatus {
        /// Capture is running.
        Recording => "recording",
        /// Capture finished; nothing has been processed yet.
        Recorded => "recorded",
        /// The pipeline (transcription, vision, notes) is running.
        Processing => "processing",
        /// The pipeline finished successfully.
        Processed => "processed",
        /// Capture or processing failed; processing can be retried.
        Failed => "failed",
    }
}

impl SessionStatus {
    /// Whether the lifecycle allows moving from `self` to `next`.
    ///
    /// ```text
    /// Recording  -> Recorded | Failed
    /// Recorded   -> Processing
    /// Processing -> Processed | Failed
    /// Processed  -> Processing      (re-run)
    /// Failed     -> Processing      (retry)
    /// ```
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Recording, Self::Recorded | Self::Failed)
                | (
                    Self::Recorded | Self::Processed | Self::Failed,
                    Self::Processing
                )
                | (Self::Processing, Self::Processed | Self::Failed)
        )
    }
}

/// A lifecycle rule was violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The requested status change is not allowed.
    InvalidTransition {
        /// Current status.
        from: SessionStatus,
        /// Requested status.
        to: SessionStatus,
    },
    /// The end time is earlier than the start time.
    EndsBeforeStart,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid session status transition: {from} -> {to}")
            }
            Self::EndsBeforeStart => f.write_str("session end time is before its start time"),
        }
    }
}

impl Error for SessionError {}

/// One recording/import and everything derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Identity.
    pub id: SessionId,
    /// What produced the session.
    pub kind: SessionKind,
    /// Human-readable title.
    pub title: String,
    /// When capture started.
    pub started_at: EpochMs,
    /// Spoken/transcription language.
    pub language: LanguageSetting,
    /// Language the notes are generated in (defaults come from settings, per-session override).
    pub note_language: LanguageCode,
    // Private on purpose: the lifecycle rules are only enforced if these two can change
    // exclusively through the transition methods (or `Session::restore` for stored data).
    ended_at: Option<EpochMs>,
    status: SessionStatus,
}

/// The persisted shape of a [`Session`], used by adapters to rebuild one via
/// [`Session::restore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Identity.
    pub id: SessionId,
    /// What produced the session.
    pub kind: SessionKind,
    /// Human-readable title.
    pub title: String,
    /// When capture started.
    pub started_at: EpochMs,
    /// When capture ended, if it did.
    pub ended_at: Option<EpochMs>,
    /// Spoken/transcription language.
    pub language: LanguageSetting,
    /// Language the notes are generated in.
    pub note_language: LanguageCode,
    /// Lifecycle status as stored.
    pub status: SessionStatus,
}

impl Session {
    /// Rebuilds a session from stored data. This is the only way to obtain a session in an
    /// arbitrary status; it checks the stored values are consistent but cannot know the
    /// history that led to them, so use the transition methods for any lifecycle change.
    pub fn restore(record: SessionRecord) -> Result<Self, SessionError> {
        if record.ended_at.is_some_and(|end| end < record.started_at) {
            return Err(SessionError::EndsBeforeStart);
        }
        Ok(Self {
            id: record.id,
            kind: record.kind,
            title: record.title,
            started_at: record.started_at,
            ended_at: record.ended_at,
            language: record.language,
            note_language: record.note_language,
            status: record.status,
        })
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> SessionStatus {
        self.status
    }

    /// When capture ended; `None` while recording.
    #[must_use]
    pub fn ended_at(&self) -> Option<EpochMs> {
        self.ended_at
    }

    /// Starts a new session in the [`SessionStatus::Recording`] state.
    #[must_use]
    pub fn start_recording(
        id: SessionId,
        kind: SessionKind,
        title: impl Into<String>,
        started_at: EpochMs,
        language: LanguageSetting,
        note_language: LanguageCode,
    ) -> Self {
        Self {
            id,
            kind,
            title: title.into(),
            started_at,
            ended_at: None,
            language,
            note_language,
            status: SessionStatus::Recording,
        }
    }

    /// Moves to `next` if the lifecycle allows it.
    pub fn transition_to(&mut self, next: SessionStatus) -> Result<(), SessionError> {
        if self.status.can_transition_to(next) {
            self.status = next;
            Ok(())
        } else {
            Err(SessionError::InvalidTransition {
                from: self.status,
                to: next,
            })
        }
    }

    /// `Recording -> Recorded`, stamping the end time.
    pub fn finish_recording(&mut self, ended_at: EpochMs) -> Result<(), SessionError> {
        if ended_at < self.started_at {
            return Err(SessionError::EndsBeforeStart);
        }
        self.transition_to(SessionStatus::Recorded)?;
        self.ended_at = Some(ended_at);
        Ok(())
    }

    /// `Recorded | Processed | Failed -> Processing`.
    pub fn start_processing(&mut self) -> Result<(), SessionError> {
        self.transition_to(SessionStatus::Processing)
    }

    /// `Processing -> Processed`.
    pub fn mark_processed(&mut self) -> Result<(), SessionError> {
        self.transition_to(SessionStatus::Processed)
    }

    /// `Recording | Processing -> Failed`.
    ///
    /// `at` is when the failure was noticed. If capture was still running it becomes the end
    /// time, so the partial recording keeps a known duration; if capture had already ended
    /// the existing end time is kept. Nothing changes when the transition is rejected.
    pub fn mark_failed(&mut self, at: EpochMs) -> Result<(), SessionError> {
        if self.ended_at.is_none() && at < self.started_at {
            return Err(SessionError::EndsBeforeStart);
        }
        self.transition_to(SessionStatus::Failed)?;
        if self.ended_at.is_none() {
            self.ended_at = Some(at);
        }
        Ok(())
    }

    /// Duration in milliseconds once capture has ended.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.ended_at
            .and_then(|end| u64::try_from(end.get().saturating_sub(self.started_at.get())).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionError, SessionKind, SessionRecord, SessionStatus};
    use crate::ids::SessionId;
    use crate::language::{LanguageCode, LanguageSetting};
    use crate::EpochMs;

    fn recording() -> Session {
        Session::start_recording(
            SessionId::new("s1"),
            SessionKind::Meeting,
            "Weekly sync",
            EpochMs::new(1_000),
            LanguageSetting::Auto,
            LanguageCode::turkish(),
        )
    }

    #[test]
    fn happy_path_walks_the_whole_lifecycle() {
        let mut s = recording();
        assert_eq!(s.status(), SessionStatus::Recording);
        s.finish_recording(EpochMs::new(4_000)).unwrap();
        assert_eq!(s.status, SessionStatus::Recorded);
        assert_eq!(s.duration_ms(), Some(3_000));
        s.start_processing().unwrap();
        s.mark_processed().unwrap();
        assert_eq!(s.status, SessionStatus::Processed);
        // Re-run is allowed.
        s.start_processing().unwrap();
        s.mark_failed(EpochMs::new(5_000)).unwrap();
        // Retry after failure is allowed.
        s.start_processing().unwrap();
    }

    #[test]
    fn recording_cannot_jump_to_processed() {
        let mut s = recording();
        assert_eq!(
            s.transition_to(SessionStatus::Processed),
            Err(SessionError::InvalidTransition {
                from: SessionStatus::Recording,
                to: SessionStatus::Processed
            })
        );
        assert_eq!(
            s.status,
            SessionStatus::Recording,
            "status unchanged on rejection"
        );
    }

    #[test]
    fn every_disallowed_pair_is_rejected() {
        use SessionStatus::{Failed, Processed, Processing, Recorded, Recording};
        let allowed = [
            (Recording, Recorded),
            (Recording, Failed),
            (Recorded, Processing),
            (Processing, Processed),
            (Processing, Failed),
            (Processed, Processing),
            (Failed, Processing),
        ];
        for &from in SessionStatus::ALL {
            for &to in SessionStatus::ALL {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn finish_recording_rejects_end_before_start_and_keeps_state() {
        let mut s = recording();
        assert_eq!(
            s.finish_recording(EpochMs::new(10)),
            Err(SessionError::EndsBeforeStart)
        );
        assert_eq!(s.status(), SessionStatus::Recording);
        assert_eq!(s.ended_at, None);
    }

    #[test]
    fn finishing_twice_is_rejected() {
        let mut s = recording();
        s.finish_recording(EpochMs::new(2_000)).unwrap();
        assert!(s.finish_recording(EpochMs::new(3_000)).is_err());
        assert_eq!(s.ended_at, Some(EpochMs::new(2_000)));
    }

    #[test]
    fn status_text_round_trips() {
        for &status in SessionStatus::ALL {
            assert_eq!(status.as_str().parse::<SessionStatus>().unwrap(), status);
        }
        assert!("bogus".parse::<SessionStatus>().is_err());
    }

    #[test]
    fn failing_a_running_recording_stamps_the_end_time() {
        let mut s = recording();
        s.mark_failed(EpochMs::new(7_000)).unwrap();
        assert_eq!(s.status(), SessionStatus::Failed);
        assert_eq!(s.ended_at(), Some(EpochMs::new(7_000)));
        assert_eq!(s.duration_ms(), Some(6_000));
    }

    #[test]
    fn failing_after_capture_keeps_the_original_end_time() {
        let mut s = recording();
        s.finish_recording(EpochMs::new(4_000)).unwrap();
        s.start_processing().unwrap();
        s.mark_failed(EpochMs::new(9_000)).unwrap();
        assert_eq!(s.ended_at(), Some(EpochMs::new(4_000)));
    }

    #[test]
    fn rejected_failure_changes_nothing() {
        let mut s = recording();
        s.finish_recording(EpochMs::new(4_000)).unwrap();
        // Recorded -> Failed is not allowed.
        assert!(s.mark_failed(EpochMs::new(5_000)).is_err());
        assert_eq!(s.status(), SessionStatus::Recorded);
        assert_eq!(s.ended_at(), Some(EpochMs::new(4_000)));

        let mut early = recording();
        assert_eq!(
            early.mark_failed(EpochMs::new(1)),
            Err(SessionError::EndsBeforeStart)
        );
        assert_eq!(early.status(), SessionStatus::Recording);
        assert_eq!(early.ended_at(), None);
    }

    #[test]
    fn restore_rebuilds_any_status_but_rejects_inconsistent_times() {
        let record = |status, ended_at| SessionRecord {
            id: SessionId::new("s1"),
            kind: SessionKind::Meeting,
            title: "t".to_owned(),
            started_at: EpochMs::new(1_000),
            ended_at,
            language: LanguageSetting::Auto,
            note_language: LanguageCode::turkish(),
            status,
        };
        let restored =
            Session::restore(record(SessionStatus::Processed, Some(EpochMs::new(2_000)))).unwrap();
        assert_eq!(restored.status(), SessionStatus::Processed);
        assert_eq!(restored.ended_at(), Some(EpochMs::new(2_000)));
        assert_eq!(
            Session::restore(record(SessionStatus::Recorded, Some(EpochMs::new(5)))),
            Err(SessionError::EndsBeforeStart)
        );
    }
}
