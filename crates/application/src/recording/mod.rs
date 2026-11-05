//! Recording use cases: start, stop and crash recovery of a capture session (ADR-009).
//!
//! The microphone and the system audio are captured as two separate tracks; the screen is
//! sampled periodically and only visibly changed frames are stored
//! ([`frame_selector`]). Everything shares one monotonic timeline whose origin is the moment
//! recording started, so a frame timestamp and a transcript offset mean the same thing.

pub mod frame_selector;
mod service;

use std::error::Error;
use std::fmt;
use std::io;

use tan_domain::{EpochMs, LanguageCode, LanguageSetting, Session, SessionId, TrackKind};

use crate::error::RepositoryError;
use crate::ports::CaptureError;

pub use frame_selector::{
    decide_frame, hamming_distance, FrameDecision, FrameSelector, FrameSelectorConfig, KeepReason,
    KeptFrame, DEFAULT_KEEP_ALIVE_MS,
};
pub use service::{RecordingPorts, RecordingService};

/// Longest accepted session title, in characters.
pub const MAX_TITLE_LEN: usize = 200;

/// How a recording is started. Anything left `None` falls back to the application settings
/// (or to the system default device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOptions {
    /// Title of the new session; must not be blank.
    pub title: String,
    /// Spoken language of the recording.
    pub speech_language: Option<LanguageSetting>,
    /// Language the notes are generated in.
    pub note_language: Option<LanguageCode>,
    /// Microphone to use (`None` = system default).
    pub microphone_device_id: Option<String>,
    /// Display to sample (`None` = primary display).
    pub display_id: Option<String>,
}

impl StartOptions {
    /// Options with only a title; everything else defaulted.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            speech_language: None,
            note_language: None,
            microphone_device_id: None,
            display_id: None,
        }
    }
}

/// A running recording as seen from outside.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveRecordingStatus {
    /// The session being recorded.
    pub session_id: SessionId,
    /// Its title.
    pub title: String,
    /// When it started (wall clock).
    pub started_at: EpochMs,
    /// Time since it started, in milliseconds.
    pub elapsed_ms: u64,
    /// Microphone level (RMS, `0.0..=1.0`) over the last update interval.
    pub microphone_level: f32,
    /// System audio level (RMS, `0.0..=1.0`) over the last update interval.
    pub system_level: f32,
    /// Frames stored so far.
    pub frame_count: u32,
}

/// Whether a recording is running.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordingStatus {
    /// Nothing is being recorded.
    Idle,
    /// A recording is running.
    Recording(ActiveRecordingStatus),
}

/// A periodic update while recording.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingProgress {
    /// The session being recorded.
    pub session_id: SessionId,
    /// Time since it started, in milliseconds.
    pub elapsed_ms: u64,
    /// Microphone level (RMS, `0.0..=1.0`).
    pub microphone_level: f32,
    /// System audio level (RMS, `0.0..=1.0`).
    pub system_level: f32,
    /// Frames stored so far.
    pub frame_count: u32,
}

/// Something that happened to a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordingEvent {
    /// Recording started.
    Started {
        /// The new session.
        session_id: SessionId,
        /// Its title.
        title: String,
    },
    /// Periodic progress (about four times a second).
    Progress(RecordingProgress),
    /// A source had a problem but recording continues (a stream failed to start, an xrun,
    /// a write error...). The text is English and free of captured content.
    Warning {
        /// The session being recorded.
        session_id: SessionId,
        /// What happened.
        message: String,
    },
    /// Recording ended (successfully or not; the session carries the outcome).
    Stopped {
        /// The session that ended.
        session_id: SessionId,
    },
}

/// Receives [`RecordingEvent`]s. Implemented by the UI shell (Tauri events, tray); called
/// from worker threads, so it must not block.
pub trait RecordingEvents: Send + Sync {
    /// Delivers one event.
    fn publish(&self, event: RecordingEvent);
}

/// Discards every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRecordingEvents;

impl RecordingEvents for NoRecordingEvents {
    fn publish(&self, _event: RecordingEvent) {}
}

/// What one audio track looked like at the end of a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSummary {
    /// Microphone or system audio.
    pub kind: TrackKind,
    /// Length of the stored file in milliseconds.
    pub duration_ms: u64,
    /// Samples per second per channel of the file.
    pub sample_rate: u32,
    /// Channels of the file.
    pub channels: u16,
    /// Highest absolute sample value seen (`0.0` means the source delivered only digital
    /// silence: a muted device or a blocked privacy permission).
    pub peak: f32,
    /// Offset of the first audio block from the recording start, in milliseconds (how late
    /// this source started; the file is padded with silence so both tracks line up).
    pub first_chunk_offset_ms: u64,
    /// Total silence inserted to keep the track on the shared timeline (start padding,
    /// gaps, tail), in milliseconds.
    pub silence_inserted_ms: u64,
    /// Audio blocks dropped because the writer could not keep up.
    pub dropped_chunks: u64,
    /// Problems the capture backend reported (xruns, device changes...).
    pub stream_issues: u32,
}

/// The outcome of [`RecordingService::stop`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingSummary {
    /// The session after stopping (`Recorded`, or `Failed` when nothing at all was captured).
    pub session: Session,
    /// The audio tracks that were stored.
    pub tracks: Vec<TrackSummary>,
    /// Frames stored.
    pub frame_count: u32,
    /// Frames dropped because processing could not keep up with capture.
    pub dropped_frames: u32,
    /// Non-fatal problems, in order of appearance.
    pub warnings: Vec<String>,
}

/// A session that was left in `Recording` by a crash and settled at startup.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredSession {
    /// The session.
    pub session_id: SessionId,
    /// Audio tracks found on disk.
    pub tracks: usize,
    /// Frame rows found in the database.
    pub frames: usize,
    /// `Recorded`, or `Failed` when neither audio nor frames survived.
    pub status: tan_domain::SessionStatus,
}

/// Failure of a recording use case. Messages are free of captured content.
#[derive(Debug)]
pub enum RecordingError {
    /// A recording is already running (or starting/stopping).
    AlreadyRecording,
    /// No recording is running.
    NotRecording,
    /// The request was rejected.
    Invalid(String),
    /// No audio source could be started; carries the first backend error.
    Capture(CaptureError),
    /// The database failed.
    Storage(RepositoryError),
    /// Session files could not be created or written.
    Io(io::Error),
}

impl fmt::Display for RecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRecording => f.write_str("a recording is already in progress"),
            Self::NotRecording => f.write_str("no recording is in progress"),
            Self::Invalid(why) => write!(f, "invalid recording request: {why}"),
            Self::Capture(e) => write!(f, "cannot capture audio: {e}"),
            // Details stay in `source()` for logs.
            Self::Storage(_) => f.write_str("the database operation failed"),
            Self::Io(_) => f.write_str("the recording files could not be written"),
        }
    }
}

impl Error for RecordingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capture(e) => Some(e),
            Self::Storage(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<RepositoryError> for RecordingError {
    fn from(e: RepositoryError) -> Self {
        Self::Storage(e)
    }
}

impl From<io::Error> for RecordingError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A language-neutral default title for a session started without one: the start time in
/// UTC, e.g. `2026-11-05 11:32 UTC` (the domain has no time zone database; the UI renames
/// sessions anyway).
#[must_use]
pub fn default_session_title(started_at: EpochMs) -> String {
    let seconds = started_at.get().div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let minute_of_day = seconds.rem_euclid(86_400) / 60;

    // Civil-from-days (Howard Hinnant's algorithm), valid for the whole i64 range we meet.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minute_of_day / 60,
        minute_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_title_is_the_utc_start_time() {
        // 2023-11-14 22:13:20 UTC
        assert_eq!(
            default_session_title(EpochMs::new(1_700_000_000_000)),
            "2023-11-14 22:13 UTC"
        );
        assert_eq!(
            default_session_title(EpochMs::new(0)),
            "1970-01-01 00:00 UTC"
        );
        // Leap day, and the second before midnight.
        assert_eq!(
            default_session_title(EpochMs::new(1_709_251_199_000)),
            "2024-02-29 23:59 UTC"
        );
        assert_eq!(
            default_session_title(EpochMs::new(1_709_251_200_000)),
            "2024-03-01 00:00 UTC"
        );
    }

    #[test]
    fn default_title_handles_times_before_the_epoch() {
        assert_eq!(
            default_session_title(EpochMs::new(-1)),
            "1969-12-31 23:59 UTC"
        );
    }
}
