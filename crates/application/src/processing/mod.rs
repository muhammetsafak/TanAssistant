//! The processing pipeline (ADR-004): turns a recorded session into a structured meeting note.
//!
//! ```text
//! Transcribe      TranscribeSession (assigned provider or local whisper)
//! DescribeFrames  one vision-model call per frame           (skipped without a vision model)
//! Synthesize      transcript + frame descriptions -> chat model -> validated note JSON
//!                 (long meetings: per-chunk working notes, then a merge)
//! ```
//!
//! Every step's result is stored as soon as it exists (transcript, frame descriptions, note),
//! and the [`tan_domain::ProcessingJob`] records which steps are finished, so a run that failed
//! or was cancelled resumes with [`ProcessSession::retry`] without repeating finished work.
//!
//! Prompts live in `prompts/*.md` and take the note language as a parameter (ADR-005). Meeting
//! content is always passed between random delimiters and the model is told to treat it as
//! data, never as instructions.
//!
//! Every error type here is safe to show to the user: none carries an API key.

mod chunking;
mod describe_frames;
mod job;
mod synthesize;

use std::error::Error;
use std::fmt;
use std::future::Future;

use tan_domain::{NoteId, ProcessingJob, ProcessingStep, SessionId, SessionStatus};

use crate::error::RepositoryError;
use crate::ports::AiError;
use crate::transcription::{
    TranscribeError, TranscribeProgress, TranscribeResult, TranscribeSummary,
};

pub use chunking::{
    build_timeline, estimate_tokens, format_offset, frame_alias, split_into_chunks, TimelineEntry,
    TokenBudget, CHARS_PER_TOKEN, PROMPT_OVERHEAD_TOKENS,
};
pub use job::{ProcessPorts, ProcessSession};

pub(crate) use chunking::{utc_date_string, utc_datetime_string};

/// Persistence of processing jobs.
pub trait ProcessingJobRepository: Send + Sync {
    /// The session's job, if one exists.
    fn get(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = Result<Option<ProcessingJob>, RepositoryError>> + Send;

    /// Inserts or replaces the session's job with all its steps.
    fn save(&self, job: &ProcessingJob)
        -> impl Future<Output = Result<(), RepositoryError>> + Send;
}

/// Runs the transcription step. The production implementation wraps
/// [`crate::transcription::TranscribeSession`]; it is a port so the pipeline can be tested
/// without audio engines.
pub trait SessionTranscriber: Send + Sync {
    /// Transcribes every audio track of the session and stores the transcript.
    fn transcribe(
        &self,
        session_id: &SessionId,
        on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
    ) -> impl Future<Output = TranscribeResult<TranscribeSummary>> + Send;
}

/// Where a run stands. Sent whenever a step starts, as its work advances, and when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessProgress {
    /// The step being worked on.
    pub step: ProcessingStep,
    /// Progress inside the step, `0..=100`.
    pub step_percent: u8,
    /// Progress of the whole run, `0..=100` (the steps are weighted by their usual cost).
    pub percent: u8,
}

/// What a finished run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSummary {
    /// The session.
    pub session_id: SessionId,
    /// The stored note.
    pub note_id: NoteId,
    /// Decisions in the note.
    pub decisions: usize,
    /// Action items in the note.
    pub action_items: usize,
    /// Frames the note refers to.
    pub key_frames: usize,
    /// Frames with a vision description after the run.
    pub frames_described: usize,
    /// Whether frame analysis was skipped because no vision model is assigned.
    pub frames_skipped: bool,
}

/// Failure of a processing run.
#[derive(Debug)]
pub enum ProcessError {
    /// The session does not exist.
    NotFound,
    /// The session is still being recorded.
    NotReady(SessionStatus),
    /// The session is already being processed by this application.
    AlreadyRunning,
    /// The run was cancelled; it can be resumed with a retry.
    Cancelled,
    /// Transcription failed.
    Transcribe(TranscribeError),
    /// No provider is assigned to the summary task.
    NoChatModel,
    /// An AI call failed (or a provider adapter could not be built).
    Provider(AiError),
    /// The chat model's reply was not a valid note, even after one repair request.
    InvalidModelOutput(String),
    /// There is neither a transcript nor a screen description to write a note from.
    NothingToSummarize,
    /// The database failed.
    Storage(RepositoryError),
    /// A lifecycle rule was broken (a bug, not a user error).
    Internal(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("session not found"),
            Self::NotReady(status) => {
                write!(f, "the session cannot be processed while it is {status}")
            }
            Self::AlreadyRunning => f.write_str("the session is already being processed"),
            Self::Cancelled => f.write_str("processing was cancelled"),
            Self::Transcribe(error) => write!(f, "{error}"),
            Self::NoChatModel => f.write_str(
                "no AI provider is assigned to the summary task; assign one in the settings",
            ),
            Self::Provider(error) => write!(f, "{error}"),
            Self::InvalidModelOutput(why) => {
                write!(f, "the model did not return a valid note: {why}")
            }
            Self::NothingToSummarize => {
                f.write_str("the session has no transcript or screen content to summarise")
            }
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Storage(_) => f.write_str("the database operation failed"),
            Self::Internal(why) => write!(f, "internal error: {why}"),
        }
    }
}

impl Error for ProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transcribe(error) => Some(error),
            Self::Provider(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RepositoryError> for ProcessError {
    fn from(error: RepositoryError) -> Self {
        Self::Storage(error)
    }
}

impl From<AiError> for ProcessError {
    fn from(error: AiError) -> Self {
        Self::Provider(error)
    }
}

impl From<TranscribeError> for ProcessError {
    fn from(error: TranscribeError) -> Self {
        Self::Transcribe(error)
    }
}

/// Result alias for processing.
pub type ProcessResult<T> = Result<T, ProcessError>;
