//! Transcription use case (ADR-003, ADR-005): turns the audio tracks of a session into a
//! time-stamped, speaker-labelled transcript.
//!
//! The speech-to-text engine is whatever the user assigned to the transcription task (a cloud
//! provider through [`TranscriptionModel`]); without an assignment the local whisper engine
//! ([`LocalTranscriber`]) is used. The audio helpers behind [`AudioChunker`] and
//! [`SessionAudioFiles`] are ports too, so this crate never touches a file or a codec.
//!
//! A cloud provider never gets a whole recording: [`chunking`] cuts each track at the pauses
//! the speech map reports into short pieces, sends them a few at a time (retrying a piece that
//! failed for a transient reason) and puts the segments back on the recording's timeline.
//!
//! Every error type here is safe to show to the user: none carries an API key.

mod chunking;
mod session;

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use tan_domain::{ProviderKind, ProviderProfileId, SessionId, TrackKind};

use crate::error::RepositoryError;
use crate::ports::{AiError, TranscriptionModel};

pub use chunking::{is_transient, plan_chunks, ChunkSettings, RetryPolicy};
pub use session::{merge_tracks, TranscribePorts, TranscribeSession};

/// Speaker label of the microphone track (the user). The UI localises it.
pub const SPEAKER_ME: &str = "me";
/// Speaker label of the system-audio track (the other participants). The UI localises it.
pub const SPEAKER_OTHERS: &str = "others";

/// Longest audio piece sent to a cloud provider, in milliseconds.
///
/// Short pieces keep every upload far below the smallest cap among the providers (`OpenAI` 25 MB,
/// Gemini about 14 MB inline) whatever the codec does, give a model that returns no timestamps
/// a useful granularity (one segment per piece) and bound the work lost when one piece fails.
pub const CLOUD_CHUNK_MS: u64 = 30_000;

/// Largest encoded piece the use case will upload, in bytes: the smallest provider cap (Gemini's
/// inline limit, 14 MB) so a piece that passes goes to every provider.
pub const CLOUD_UPLOAD_LIMIT_BYTES: usize = 14 * 1024 * 1024;

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

/// Where the speech is in a recording, as the voice-activity detector sees it. All times are
/// milliseconds from the start of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechMap {
    /// Length of the recording.
    pub duration_ms: u64,
    /// Stretches that hold speech (padded and bridged by the detector), in time order. Silence
    /// is everything between them.
    pub regions: Vec<AudioSpan>,
    /// Loudness (RMS) of consecutive windows of the whole recording; used to cut inside a
    /// stretch of speech that is longer than a piece, at its quietest moment.
    pub energies: Vec<f32>,
    /// Length of one window of `energies`.
    pub energy_window_ms: u64,
}

/// How a piece is encoded for upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFormat {
    /// The adapter's preferred compressed format (small, lossless where it can be).
    Compressed,
    /// Plain 16 kHz mono 16-bit WAV: bigger (about 1 MB for a 30-second piece) but accepted by
    /// every provider. The fallback when a provider refuses the compressed format.
    Wav,
}

/// A piece of audio ready to upload.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedAudio {
    /// File name including the extension (providers sniff the format from it).
    pub file_name: String,
    /// MIME type, e.g. `audio/flac`.
    pub mime_type: String,
    /// The encoded file.
    pub data: Vec<u8>,
}

impl fmt::Debug for EncodedAudio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodedAudio")
            .field("file_name", &self.file_name)
            .field("mime_type", &self.mime_type)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// Prepares recordings for a cloud provider: finds the speech and encodes pieces of it.
pub trait AudioChunker: Send + Sync {
    /// Runs voice-activity detection over the whole file.
    fn speech_map(
        &self,
        audio: &Path,
    ) -> impl Future<Output = Result<SpeechMap, AudioPrepError>> + Send;

    /// The span as a 16 kHz mono file in the requested format.
    fn encode(
        &self,
        audio: &Path,
        span: AudioSpan,
        format: UploadFormat,
    ) -> impl Future<Output = Result<EncodedAudio, AudioPrepError>> + Send;
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

/// Where the pieces of one track stand while a cloud provider transcribes it. Sent when the
/// pieces are planned and after each one finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkProgress {
    /// The track being transcribed.
    pub track: TrackKind,
    /// Pieces finished.
    pub chunks_done: usize,
    /// Pieces in the track.
    pub chunks_total: usize,
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
