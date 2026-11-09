//! IPC commands for the processing pipeline: start, retry, cancel and inspect a run.
//!
//! Thin adapters over `ProcessSession`. A run can take minutes; its progress is pushed to the UI
//! as [`ProcessingProgressEvent`]s and the command returns when the run ends. Cancelling comes
//! through `cancel_processing`, which makes the pending `process_session` call return a
//! `cancelled` error.

use std::error::Error;

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::ports::AiError;
use tan_application::processing::{ProcessError, ProcessProgress, ProcessSummary};
use tan_domain::{ProcessingJob, ProcessingStep, ProviderProfileId, SessionId, StepState};
use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::commands::transcription::{TranscriptionErrorCode, TranscriptionIpcError};
use crate::commands::updates::{gated, StartKind, UpdateInProgress, UpdaterState};
use crate::state::AppState;

/// Machine-readable failure class of the processing commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingErrorCode {
    /// The input was rejected.
    Validation,
    /// The session does not exist.
    SessionNotFound,
    /// The session is still being recorded.
    NotReady,
    /// The session is already being processed.
    AlreadyRunning,
    /// The run was cancelled; `retry_processing` continues it.
    Cancelled,
    /// Transcription failed; see `transcriptionCode` for the reason.
    TranscriptionFailed,
    /// No AI provider is assigned to the summary task.
    NoChatModel,
    /// The model's reply was not a valid note, even after a repair request.
    InvalidModelOutput,
    /// The session has no transcript or screen content to write a note from.
    NothingToSummarize,
    /// The provider rejected the API key.
    ProviderAuth,
    /// The provider is rate limiting.
    ProviderRateLimited,
    /// The provider did not answer in time.
    ProviderTimeout,
    /// The provider refused the request.
    ProviderInvalidRequest,
    /// The provider cannot do what was asked.
    ProviderUnsupported,
    /// The provider could not be reached.
    ProviderNetwork,
    /// The provider answered with another error.
    ProviderError,
    /// The API key could not be read from the secret store.
    SecretUnavailable,
    /// An update is being installed (or waits for the restart): no processing run can start
    /// until the application restarts.
    UpdateInProgress,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned by the processing commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingIpcError {
    /// Failure class.
    pub code: ProcessingErrorCode,
    /// For `transcription_failed`: the transcription failure class (shared with
    /// `transcribe_session`), so the UI can reuse its messages.
    pub transcription_code: Option<TranscriptionErrorCode>,
    /// Human-readable, key-free description (English; the UI localises by `code`).
    pub message: String,
}

impl ProcessingIpcError {
    fn validation(message: &str) -> Self {
        Self {
            code: ProcessingErrorCode::Validation,
            transcription_code: None,
            message: message.to_owned(),
        }
    }
}

fn log_source(context: &str, error: &(dyn Error + 'static)) {
    // Walk the chain by hand so only `Display` strings are logged.
    let mut chain = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push(cause.to_string());
        source = cause.source();
    }
    tracing::error!(error = %chain.join(": "), "{context}");
}

const fn ai_error_code(error: &AiError) -> ProcessingErrorCode {
    match error {
        AiError::Auth => ProcessingErrorCode::ProviderAuth,
        AiError::RateLimited { .. } => ProcessingErrorCode::ProviderRateLimited,
        AiError::Timeout => ProcessingErrorCode::ProviderTimeout,
        AiError::InvalidRequest(_) => ProcessingErrorCode::ProviderInvalidRequest,
        AiError::Unsupported(_) => ProcessingErrorCode::ProviderUnsupported,
        AiError::Network(_) => ProcessingErrorCode::ProviderNetwork,
        AiError::Provider { .. } => ProcessingErrorCode::ProviderError,
        AiError::SecretUnavailable(_) => ProcessingErrorCode::SecretUnavailable,
    }
}

impl From<UpdateInProgress> for ProcessingIpcError {
    fn from(error: UpdateInProgress) -> Self {
        Self {
            code: ProcessingErrorCode::UpdateInProgress,
            transcription_code: None,
            message: error.to_string(),
        }
    }
}

impl From<ProcessError> for ProcessingIpcError {
    fn from(error: ProcessError) -> Self {
        let code = match error {
            ProcessError::NotFound => ProcessingErrorCode::SessionNotFound,
            ProcessError::NotReady(_) => ProcessingErrorCode::NotReady,
            ProcessError::AlreadyRunning => ProcessingErrorCode::AlreadyRunning,
            ProcessError::Cancelled => ProcessingErrorCode::Cancelled,
            ProcessError::Transcribe(inner) => {
                // Same classification (and logging) as the standalone transcription command.
                let mapped = TranscriptionIpcError::from(inner);
                return Self {
                    code: ProcessingErrorCode::TranscriptionFailed,
                    transcription_code: Some(mapped.code),
                    message: mapped.message,
                };
            }
            ProcessError::UnknownTranscriptionProfile => ProcessingErrorCode::Validation,
            ProcessError::NoChatModel => ProcessingErrorCode::NoChatModel,
            ProcessError::Provider(ref inner) => ai_error_code(inner),
            ProcessError::InvalidModelOutput(_) => ProcessingErrorCode::InvalidModelOutput,
            ProcessError::NothingToSummarize => ProcessingErrorCode::NothingToSummarize,
            ProcessError::Storage(_) | ProcessError::Internal(_) => {
                log_source("processing failed", &error);
                ProcessingErrorCode::Internal
            }
        };
        Self {
            code,
            transcription_code: None,
            message: error.to_string(),
        }
    }
}

/// A step of the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStepDto {
    /// Speech-to-text.
    Transcribe,
    /// Vision descriptions of the screen frames.
    DescribeFrames,
    /// The structured note.
    Synthesize,
}

impl From<ProcessingStep> for ProcessingStepDto {
    fn from(step: ProcessingStep) -> Self {
        match step {
            ProcessingStep::Transcribe => Self::Transcribe,
            ProcessingStep::DescribeFrames => Self::DescribeFrames,
            _ => Self::Synthesize,
        }
    }
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StepStateDto {
    /// Not started.
    Pending,
    /// Running (or interrupted).
    Running,
    /// Finished.
    Done,
    /// Deliberately not run (no vision model assigned); counts as finished.
    Skipped,
    /// Stopped with an error; see the step's `error`.
    Failed,
}

/// One step of a processing job.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StepStatusDto {
    /// The step.
    pub step: ProcessingStepDto,
    /// Its state.
    pub state: StepStateDto,
    /// Why it failed, for `failed`.
    pub error: Option<String>,
}

/// How far processing got for a session.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingJobDto {
    /// The session.
    pub session_id: String,
    /// The steps in execution order.
    pub steps: Vec<StepStatusDto>,
    /// When the job was created, milliseconds since the Unix epoch.
    pub created_at: f64,
    /// When a step last changed.
    pub updated_at: f64,
    /// When the last step finished; `null` while unfinished.
    pub finished_at: Option<f64>,
}

/// JS numbers are doubles and specta refuses 64-bit integers.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn epoch_ms_to_f64(at: tan_domain::EpochMs) -> f64 {
    at.get() as f64
}

impl From<&ProcessingJob> for ProcessingJobDto {
    fn from(job: &ProcessingJob) -> Self {
        Self {
            session_id: job.session_id().to_string(),
            steps: job
                .steps()
                .iter()
                .map(|(step, state)| StepStatusDto {
                    step: (*step).into(),
                    state: match state {
                        StepState::Pending => StepStateDto::Pending,
                        StepState::Running => StepStateDto::Running,
                        StepState::Done => StepStateDto::Done,
                        StepState::Skipped => StepStateDto::Skipped,
                        StepState::Failed { .. } => StepStateDto::Failed,
                    },
                    error: match state {
                        StepState::Failed { error } => Some(error.clone()),
                        _ => None,
                    },
                })
                .collect(),
            created_at: epoch_ms_to_f64(job.created_at()),
            updated_at: epoch_ms_to_f64(job.updated_at()),
            finished_at: job.finished_at().map(epoch_ms_to_f64),
        }
    }
}

/// Whether a session is being processed and how far its job got.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingStatusDto {
    /// Whether this application is processing the session right now.
    pub running: bool,
    /// The job, if the session was ever processed.
    pub job: Option<ProcessingJobDto>,
}

/// What a finished run produced.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSummaryDto {
    /// The session.
    pub session_id: String,
    /// The stored note.
    pub note_id: String,
    /// Decisions in the note.
    pub decisions: u32,
    /// Action items in the note.
    pub action_items: u32,
    /// Frames the note refers to.
    pub key_frames: u32,
    /// Frames with a vision description.
    pub frames_described: u32,
    /// Whether frame analysis was skipped because no vision model is assigned.
    pub frames_skipped: bool,
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl From<ProcessSummary> for ProcessSummaryDto {
    fn from(summary: ProcessSummary) -> Self {
        Self {
            session_id: summary.session_id.to_string(),
            note_id: summary.note_id.to_string(),
            decisions: saturating_u32(summary.decisions),
            action_items: saturating_u32(summary.action_items),
            key_frames: saturating_u32(summary.key_frames),
            frames_described: saturating_u32(summary.frames_described),
            frames_skipped: summary.frames_skipped,
        }
    }
}

/// Emitted as a processing run advances (when a step starts, as it works, when it ends).
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingProgressEvent {
    /// The session being processed.
    pub session_id: String,
    /// The step being worked on.
    pub step: ProcessingStepDto,
    /// Progress inside the step, `0..=100`.
    pub step_percent: u32,
    /// Progress of the whole run, `0..=100`.
    pub percent: u32,
}

fn parse_session_id(session_id: String) -> Result<SessionId, ProcessingIpcError> {
    if session_id.trim().is_empty() {
        return Err(ProcessingIpcError::validation(
            "invalid input: the session id is empty",
        ));
    }
    Ok(SessionId::new(session_id))
}

/// A progress callback that forwards to the UI.
fn announcer(app: AppHandle, session_id: &SessionId) -> impl Fn(ProcessProgress) + Send + Sync {
    let session_id = session_id.to_string();
    move |progress: ProcessProgress| {
        let event = ProcessingProgressEvent {
            session_id: session_id.clone(),
            step: progress.step.into(),
            step_percent: u32::from(progress.step_percent),
            percent: u32::from(progress.percent),
        };
        if let Err(error) = event.emit(&app) {
            tracing::debug!(%error, "could not deliver a processing event to the UI");
        }
    }
}

fn parse_profile_id(
    profile_id: Option<String>,
) -> Result<Option<ProviderProfileId>, ProcessingIpcError> {
    match profile_id {
        None => Ok(None),
        Some(id) if id.trim().is_empty() => Err(ProcessingIpcError::validation(
            "invalid input: the transcription profile id is empty",
        )),
        Some(id) => Ok(Some(ProviderProfileId::new(id))),
    }
}

/// Processes a recorded session from scratch: transcript, frame descriptions and the
/// structured note (summary, decisions, action items, open questions, key frames). Replaces an
/// earlier note. Progress arrives as [`ProcessingProgressEvent`]s; the call returns when the
/// run ends.
///
/// `transcription_profile_id` makes the cloud provider of that profile transcribe this session
/// in this run only; the general transcription assignment is not changed and applies again to
/// the next run. A profile that does not exist is a `validation` error, one that cannot
/// transcribe audio a `transcription_failed` error (`transcriptionCode` `provider_unsupported`).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn process_session(
    app: AppHandle,
    state: State<'_, AppState>,
    updater_state: State<'_, UpdaterState>,
    session_id: String,
    transcription_profile_id: Option<String>,
) -> Result<ProcessSummaryDto, ProcessingIpcError> {
    // Refused while an update installs; the permit is held for the whole run, so the install
    // sees it as active.
    gated(
        &updater_state,
        StartKind::Processing,
        Box::pin(async {
            let id = parse_session_id(session_id)?;
            let profile = parse_profile_id(transcription_profile_id)?;
            let announce = announcer(app, &id);
            let summary = state
                .process_session
                .execute_with_profile(&id, profile.as_ref(), &announce)
                .await?;
            Ok(summary.into())
        }),
    )
    .await
}

/// Continues a failed or cancelled run: finished steps are kept, the rest runs again.
///
/// The run keeps the transcription profile it was started with; `transcription_profile_id`
/// replaces it (same rules as in `process_session`).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn retry_processing(
    app: AppHandle,
    state: State<'_, AppState>,
    updater_state: State<'_, UpdaterState>,
    session_id: String,
    transcription_profile_id: Option<String>,
) -> Result<ProcessSummaryDto, ProcessingIpcError> {
    gated(
        &updater_state,
        StartKind::Processing,
        Box::pin(async {
            let id = parse_session_id(session_id)?;
            let profile = parse_profile_id(transcription_profile_id)?;
            let announce = announcer(app, &id);
            let summary = state
                .process_session
                .retry_with_profile(&id, profile.as_ref(), &announce)
                .await?;
            Ok(summary.into())
        }),
    )
    .await
}

/// Cancels the session's running run (its `process_session` call returns `cancelled`). Returns
/// whether there was a run to cancel.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn cancel_processing(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, ProcessingIpcError> {
    let id = parse_session_id(session_id)?;
    Ok(state.process_session.cancel(&id))
}

/// Whether a session is being processed and how far its job got.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn processing_status(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ProcessingStatusDto, ProcessingIpcError> {
    let id = parse_session_id(session_id)?;
    let job = state.process_session.status(&id).await?;
    Ok(ProcessingStatusDto {
        running: state.process_session.is_running(&id),
        job: job.as_ref().map(ProcessingJobDto::from),
    })
}
