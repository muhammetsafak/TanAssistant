//! IPC commands for transcription: the whisper model files and `transcribe_session`.
//!
//! Thin adapters: map DTOs to the model manager / use case and back. Download progress is
//! pushed to the UI as [`WhisperModelDownloadEvent`]s.

use std::error::Error;

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::ports::AiError;
use tan_application::transcription::{
    TranscribeError, TranscribeProgress, TranscribeSummary, TranscriptionEngine,
    TranscriptionEstimate,
};
use tan_domain::SessionId;
use tan_infra_transcription::{DownloadProgress, ModelError, ModelInfo};
use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::commands::ai::ProviderKindDto;
use crate::state::AppState;

/// Machine-readable failure class of the transcription commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionErrorCode {
    /// The input was rejected.
    Validation,
    /// The whisper model id is not in the catalog.
    UnknownModel,
    /// The model is being downloaded or deleted right now.
    ModelBusy,
    /// The model is not installed (it cannot be selected).
    ModelNotInstalled,
    /// The download failed (network or server); retrying resumes it.
    DownloadFailed,
    /// The downloaded file failed its checksum and was deleted.
    ChecksumMismatch,
    /// The session does not exist.
    SessionNotFound,
    /// The session has no audio track.
    NoAudio,
    /// Local whisper is in use but no model is installed.
    LocalModelMissing,
    /// An audio file is missing or unreadable.
    AudioUnreadable,
    /// The local engine failed.
    EngineFailed,
    /// The provider rejected the API key.
    ProviderAuth,
    /// The provider is rate limiting.
    ProviderRateLimited,
    /// The provider did not answer in time.
    ProviderTimeout,
    /// The provider refused the request.
    ProviderInvalidRequest,
    /// The provider does not offer transcription.
    ProviderUnsupported,
    /// The provider could not be reached.
    ProviderNetwork,
    /// The provider answered with another error.
    ProviderError,
    /// The API key could not be read from the secret store.
    SecretUnavailable,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned by the transcription commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionIpcError {
    /// Failure class.
    pub code: TranscriptionErrorCode,
    /// Human-readable, key-free description (English; the UI localises by `code`).
    pub message: String,
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

const fn ai_error_code(error: &AiError) -> TranscriptionErrorCode {
    match error {
        AiError::Auth => TranscriptionErrorCode::ProviderAuth,
        AiError::RateLimited { .. } => TranscriptionErrorCode::ProviderRateLimited,
        AiError::Timeout => TranscriptionErrorCode::ProviderTimeout,
        AiError::InvalidRequest(_) => TranscriptionErrorCode::ProviderInvalidRequest,
        // A CLI provider cannot transcribe, so `CliUnavailable` is not reachable in practice.
        AiError::Unsupported(_) | AiError::CliUnavailable(_) => {
            TranscriptionErrorCode::ProviderUnsupported
        }
        AiError::Network(_) => TranscriptionErrorCode::ProviderNetwork,
        AiError::Provider { .. } => TranscriptionErrorCode::ProviderError,
        AiError::SecretUnavailable(_) => TranscriptionErrorCode::SecretUnavailable,
    }
}

impl From<TranscribeError> for TranscriptionIpcError {
    fn from(error: TranscribeError) -> Self {
        let code = match &error {
            TranscribeError::NotFound => TranscriptionErrorCode::SessionNotFound,
            TranscribeError::NoAudio => TranscriptionErrorCode::NoAudio,
            TranscribeError::LocalModelMissing => TranscriptionErrorCode::LocalModelMissing,
            TranscribeError::Audio(_) => TranscriptionErrorCode::AudioUnreadable,
            // Local whisper reports its own failures as `Provider { status: 0 }`.
            TranscribeError::Engine {
                error: AiError::Provider { status: 0, .. },
                ..
            } => TranscriptionErrorCode::EngineFailed,
            TranscribeError::Engine { error, .. } | TranscribeError::Provider(error) => {
                ai_error_code(error)
            }
            TranscribeError::Storage(_) => {
                log_source("transcription failed", &error);
                TranscriptionErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

impl From<ModelError> for TranscriptionIpcError {
    fn from(error: ModelError) -> Self {
        let code = match &error {
            ModelError::UnknownModel(_) => TranscriptionErrorCode::UnknownModel,
            ModelError::Busy(_) => TranscriptionErrorCode::ModelBusy,
            ModelError::NotInstalled(_) => TranscriptionErrorCode::ModelNotInstalled,
            ModelError::Network(_)
            | ModelError::HttpStatus(_)
            | ModelError::TooLarge
            | ModelError::Incomplete { .. } => TranscriptionErrorCode::DownloadFailed,
            ModelError::ChecksumMismatch(_) => TranscriptionErrorCode::ChecksumMismatch,
            ModelError::Io(_) => {
                log_source("model file operation failed", &error);
                TranscriptionErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

/// Byte counts are sent as `u32` (the largest catalog model is about 1.1 GB); larger values
/// saturate.
fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A downloadable whisper model and its state on disk.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModelDto {
    /// Stable identifier, used by the other commands.
    pub id: String,
    /// Name shown to the user.
    pub display_name: String,
    /// Size of the model file in bytes.
    pub size_bytes: u32,
    /// Quantization of the weights (`q5_0`, `q8_0`, ...).
    pub quantization: String,
    /// The recommended default for Turkish.
    pub recommended: bool,
    /// Whether the verified model is installed.
    pub installed: bool,
    /// Whether local transcription uses this model.
    pub selected: bool,
    /// Bytes of an interrupted download waiting to be resumed (0 when none).
    pub partial_bytes: u32,
}

impl From<ModelInfo> for WhisperModelDto {
    fn from(info: ModelInfo) -> Self {
        Self {
            id: info.spec.id.to_owned(),
            display_name: info.spec.display_name.to_owned(),
            size_bytes: saturating_u32(info.spec.size_bytes),
            quantization: info.spec.quantization.to_owned(),
            recommended: info.spec.recommended,
            installed: info.installed,
            selected: info.selected,
            partial_bytes: saturating_u32(info.partial_bytes),
        }
    }
}

/// Emitted while a model downloads (about five times a second, plus at the start and end).
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModelDownloadEvent {
    /// The model being downloaded.
    pub model_id: String,
    /// Bytes on disk so far.
    pub downloaded: u32,
    /// Total size of the model.
    pub total: u32,
}

/// Emitted before each track of a transcription starts and when it is done.
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionProgressEvent {
    /// The session being transcribed.
    pub session_id: String,
    /// The track being started (`microphone` or `system`); `null` when everything is done.
    pub track: Option<String>,
    /// Tracks finished so far.
    pub tracks_done: u32,
    /// Tracks in the session.
    pub tracks_total: u32,
}

impl TranscriptionProgressEvent {
    fn new(session_id: &str, progress: TranscribeProgress) -> Self {
        Self {
            session_id: session_id.to_owned(),
            track: progress.track.map(|t| t.to_string()),
            tracks_done: u32::try_from(progress.tracks_done).unwrap_or(u32::MAX),
            tracks_total: u32::try_from(progress.tracks_total).unwrap_or(u32::MAX),
        }
    }
}

/// A forecast of how long transcribing a session will take.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionEstimateDto {
    /// The engine that would be used.
    pub engine: TranscriptionEngineDto,
    /// For a cloud engine: which provider.
    pub provider_kind: Option<ProviderKindDto>,
    /// Estimated seconds; `null` for a cloud provider (it depends on the service).
    pub estimated_seconds: Option<u32>,
    /// What does the work: `cpu`, `vulkan (<device>)`, `metal`, `cuda` or `cloud`. A forecast
    /// on `cpu` is where the UI offers to switch to a cloud provider.
    pub backend: String,
    /// Total audio of the session, in seconds.
    pub audio_seconds: u32,
    /// Speech in it after silence is skipped, in seconds (local engine only).
    pub speech_seconds: Option<u32>,
}

impl From<TranscriptionEstimate> for TranscriptionEstimateDto {
    fn from(estimate: TranscriptionEstimate) -> Self {
        let (engine, provider_kind) = match estimate.engine {
            TranscriptionEngine::LocalWhisper => (TranscriptionEngineDto::LocalWhisper, None),
            TranscriptionEngine::Provider { kind, .. } => {
                (TranscriptionEngineDto::Provider, Some(kind.into()))
            }
        };
        let seconds = |ms: u64| u32::try_from(ms / 1_000).unwrap_or(u32::MAX);
        Self {
            engine,
            provider_kind,
            estimated_seconds: estimate
                .estimated_seconds
                .map(|s| u32::try_from(s).unwrap_or(u32::MAX)),
            backend: estimate.backend,
            audio_seconds: seconds(estimate.audio_ms),
            speech_seconds: estimate.speech_ms.map(seconds),
        }
    }
}

/// Which engine produced a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionEngineDto {
    /// Local whisper (the default).
    LocalWhisper,
    /// A cloud provider the user assigned.
    Provider,
}

/// What a finished transcription produced.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeSummaryDto {
    /// The session.
    pub session_id: String,
    /// Segments stored.
    pub segment_count: u32,
    /// The engine that was used.
    pub engine: TranscriptionEngineDto,
    /// For a cloud engine: which provider.
    pub provider_kind: Option<ProviderKindDto>,
    /// For a cloud engine: the profile that was used.
    pub profile_id: Option<String>,
}

impl From<TranscribeSummary> for TranscribeSummaryDto {
    fn from(summary: TranscribeSummary) -> Self {
        let (engine, provider_kind, profile_id) = match summary.engine {
            TranscriptionEngine::LocalWhisper => (TranscriptionEngineDto::LocalWhisper, None, None),
            TranscriptionEngine::Provider { profile_id, kind } => (
                TranscriptionEngineDto::Provider,
                Some(kind.into()),
                Some(profile_id.to_string()),
            ),
        };
        Self {
            session_id: summary.session_id.to_string(),
            segment_count: u32::try_from(summary.segment_count).unwrap_or(u32::MAX),
            engine,
            provider_kind,
            profile_id,
        }
    }
}

/// Lists the whisper models Tan can download, best choice first, with their state on disk.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn list_whisper_models(
    state: State<'_, AppState>,
) -> Result<Vec<WhisperModelDto>, TranscriptionIpcError> {
    Ok(state
        .whisper_models
        .list()
        .into_iter()
        .map(WhisperModelDto::from)
        .collect())
}

/// Downloads a whisper model and verifies its checksum. Progress arrives as
/// [`WhisperModelDownloadEvent`]s; an interrupted download resumes when called again.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn download_whisper_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> Result<WhisperModelDto, TranscriptionIpcError> {
    let announce = {
        let model_id = model_id.clone();
        move |progress: DownloadProgress| {
            let event = WhisperModelDownloadEvent {
                model_id: model_id.clone(),
                downloaded: saturating_u32(progress.downloaded),
                total: saturating_u32(progress.total),
            };
            if let Err(error) = event.emit(&app) {
                tracing::debug!(%error, "could not deliver a download event to the UI");
            }
        }
    };
    state.whisper_models.download(&model_id, announce).await?;
    state
        .whisper_models
        .list()
        .into_iter()
        .find(|info| info.spec.id == model_id)
        .map(WhisperModelDto::from)
        .ok_or_else(|| ModelError::UnknownModel(model_id).into())
}

/// Chooses which installed whisper model local transcription uses.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn select_whisper_model(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<(), TranscriptionIpcError> {
    Ok(state.whisper_models.select(&model_id)?)
}

/// Deletes a whisper model (and any partial download of it). Returns whether anything was
/// removed.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn delete_whisper_model(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<bool, TranscriptionIpcError> {
    Ok(state.whisper_models.delete(&model_id)?)
}

/// Transcribes the audio tracks of a session with the provider assigned to the transcription
/// task, or local whisper when none is assigned, and stores the transcript. This can take
/// minutes for a long recording; the processing pipeline calls the same use case.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn transcribe_session(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<TranscribeSummaryDto, TranscriptionIpcError> {
    if session_id.trim().is_empty() {
        return Err(TranscriptionIpcError {
            code: TranscriptionErrorCode::Validation,
            message: "invalid input: the session id is empty".to_owned(),
        });
    }
    let announce = {
        let session_id = session_id.clone();
        move |progress: TranscribeProgress| {
            if let Err(error) = TranscriptionProgressEvent::new(&session_id, progress).emit(&app) {
                tracing::debug!(%error, "could not deliver a transcription event to the UI");
            }
        }
    };
    let summary = state
        .transcribe_session
        .execute_with_progress(&SessionId::new(session_id), &announce)
        .await?;
    Ok(summary.into())
}

/// Forecasts how long `transcribe_session` would take for a session: the assigned provider
/// gets no time (it depends on the service); local whisper is forecast from a short
/// calibration run on the selected model (the first call takes a few seconds, the result is
/// remembered) and from the amount of speech in the tracks.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn estimate_transcription(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<TranscriptionEstimateDto, TranscriptionIpcError> {
    if session_id.trim().is_empty() {
        return Err(TranscriptionIpcError {
            code: TranscriptionErrorCode::Validation,
            message: "invalid input: the session id is empty".to_owned(),
        });
    }
    let estimate = state
        .transcribe_session
        .estimate(&SessionId::new(session_id))
        .await?;
    Ok(estimate.into())
}
