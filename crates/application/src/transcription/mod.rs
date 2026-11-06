//! Transcription use case (ADR-003, ADR-005): turns the audio tracks of a session into a
//! time-stamped, speaker-labelled transcript.
//!
//! The speech-to-text engine is whatever the user assigned to the transcription task (a cloud
//! provider through [`TranscriptionModel`]); without an assignment the local whisper engine
//! ([`LocalTranscriber`]) is used. The audio helpers behind [`AudioChunker`] and
//! [`SessionAudioFiles`] are ports too, so this crate never touches a file or a codec.
//!
//! Every error type here is safe to show to the user: none carries an API key.

mod session;

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use tan_domain::{ProviderKind, ProviderProfileId, SessionId, TrackKind};

use crate::error::RepositoryError;
use crate::ports::{AiError, TranscriptionModel};

pub use session::{merge_tracks, TranscribePorts, TranscribeSession};

/// Speaker label of the microphone track (the user). The UI localises it.
pub const SPEAKER_ME: &str = "me";
/// Speaker label of the system-audio track (the other participants). The UI localises it.
pub const SPEAKER_OTHERS: &str = "others";

/// Longest audio piece sent to a cloud provider, in milliseconds.
///
/// Pieces are 16 kHz mono 16-bit WAV (about 1.9 MB per minute), so six minutes stay far below
/// the smallest upload cap among the providers (Gemini, about 14 MB inline).
pub const CLOUD_CHUNK_MS: u64 = 6 * 60 * 1_000;

/// Whisper transcribes in windows of this length, whatever they hold (a shorter piece is padded
/// to it), so its running time is per window, not per second of audio.
pub const WHISPER_WINDOW_MS: u64 = 30_000;

/// The speaker label of a track.
#[must_use]
pub const fn speaker_label(track: TrackKind) -> &'static str {
    match track {
        TrackKind::Microphone => SPEAKER_ME,
        TrackKind::System => SPEAKER_OTHERS,
    }
}

/// A stretch of an audio file, in milliseconds from the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSpan {
    /// Start of the stretch.
    pub start_ms: u64,
    /// End of the stretch (exclusive).
    pub end_ms: u64,
}

/// Audio could not be read or converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPrepError(pub String);

impl fmt::Display for AudioPrepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "audio could not be prepared: {}", self.0)
    }
}

impl Error for AudioPrepError {}

/// Splits long recordings into pieces a cloud provider accepts.
pub trait AudioChunker: Send + Sync {
    /// Plans the pieces of the file: spans of at most `max_chunk_ms`, cut at quiet moments so
    /// words are not split. A short file yields one span covering all of it.
    fn plan(
        &self,
        audio: &Path,
        max_chunk_ms: u64,
    ) -> impl Future<Output = Result<Vec<AudioSpan>, AudioPrepError>> + Send;

    /// The span as a 16 kHz mono 16-bit WAV file.
    fn load_wav(
        &self,
        audio: &Path,
        span: AudioSpan,
    ) -> impl Future<Output = Result<Vec<u8>, AudioPrepError>> + Send;
}

/// Resolves a stored track path (relative to the session directory) to a file on disk.
pub trait SessionAudioFiles: Send + Sync {
    /// The absolute path of `relative_path` inside the session's directory. A path that leaves
    /// the session directory is an error.
    fn audio_path(&self, session_id: &SessionId, relative_path: &str) -> io::Result<PathBuf>;
}

/// Whether the local engine can transcribe right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEngineStatus {
    /// A model is installed.
    Ready,
    /// No model is installed; the user has to download one first.
    ModelMissing,
}

/// How fast the local engine is on this machine, from a short calibration run.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSpeed {
    /// Seconds one 30-second window takes (model, backend and thread count as installed).
    pub seconds_per_window: f64,
    /// The backend that does the work: `cpu`, `vulkan (<device>)`, `metal`, `cuda`.
    pub backend: String,
}

/// The local speech-to-text engine (whisper). Reads the file itself, so a request carries
/// [`crate::ports::AudioSource::Path`], skips silence and splits long audio on its own.
pub trait LocalTranscriber: TranscriptionModel {
    /// Whether a model is installed.
    fn status(&self) -> impl Future<Output = LocalEngineStatus> + Send;

    /// Calibrates (once, then remembered) and reports the engine's speed. Fails when no model
    /// is installed.
    fn speed(&self) -> impl Future<Output = Result<LocalSpeed, AiError>> + Send;

    /// How many milliseconds of speech the file holds (what the engine would transcribe after
    /// skipping silence).
    fn speech_ms(&self, audio: &Path) -> impl Future<Output = Result<u64, AudioPrepError>> + Send;
}

/// Where a transcription stands. Sent before each track starts and once when all are done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscribeProgress {
    /// The track being started; `None` when everything is done.
    pub track: Option<TrackKind>,
    /// Tracks finished so far.
    pub tracks_done: usize,
    /// Tracks in the session.
    pub tracks_total: usize,
}

/// A forecast of how long transcribing a session will take.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionEstimate {
    /// The engine that would be used.
    pub engine: TranscriptionEngine,
    /// Estimated seconds; `None` for a cloud provider (it depends on the service).
    pub estimated_seconds: Option<u64>,
    /// The backend: `cpu`, `vulkan (<device>)`, `metal`, `cuda`, or `cloud`.
    pub backend: String,
    /// Total audio of the session's tracks, in milliseconds.
    pub audio_ms: u64,
    /// Speech in it after silence is skipped (local engine only).
    pub speech_ms: Option<u64>,
}

/// Which engine produced a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptionEngine {
    /// Local whisper (the default).
    LocalWhisper,
    /// A cloud provider the user assigned.
    Provider {
        /// The profile that was used.
        profile_id: ProviderProfileId,
        /// Which provider it is.
        kind: ProviderKind,
    },
}

/// What a finished transcription produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeSummary {
    /// The session.
    pub session_id: SessionId,
    /// Segments stored.
    pub segment_count: usize,
    /// The engine that was used.
    pub engine: TranscriptionEngine,
}

/// Failure of [`TranscribeSession`].
#[derive(Debug)]
pub enum TranscribeError {
    /// The session does not exist.
    NotFound,
    /// The session has no audio track to transcribe.
    NoAudio,
    /// Local whisper is the engine but no model is installed.
    LocalModelMissing,
    /// An audio file could not be found, read or converted.
    Audio(String),
    /// The engine failed on a track.
    Engine {
        /// The track being transcribed.
        track: TrackKind,
        /// What went wrong.
        error: AiError,
    },
    /// The assigned provider cannot transcribe, or its adapter could not be built.
    Provider(AiError),
    /// The database failed.
    Storage(RepositoryError),
}

impl fmt::Display for TranscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("session not found"),
            Self::NoAudio => f.write_str("the session has no audio to transcribe"),
            Self::LocalModelMissing => {
                f.write_str("no local whisper model is installed; download one first")
            }
            Self::Audio(why) => write!(f, "{why}"),
            Self::Engine { track, error } => {
                write!(f, "transcribing the {track} track failed: {error}")
            }
            Self::Provider(error) => write!(f, "{error}"),
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Storage(_) => f.write_str("the database operation failed"),
        }
    }
}

impl Error for TranscribeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Engine { error, .. } | Self::Provider(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RepositoryError> for TranscribeError {
    fn from(error: RepositoryError) -> Self {
        Self::Storage(error)
    }
}

impl From<AudioPrepError> for TranscribeError {
    fn from(error: AudioPrepError) -> Self {
        Self::Audio(error.to_string())
    }
}

/// Result alias for transcription.
pub type TranscribeResult<T> = Result<T, TranscribeError>;
