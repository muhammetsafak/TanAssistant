//! IPC commands for AI provider profiles, the connection test and task assignments.
//!
//! Thin adapters: map DTOs to use-case input, call the use case, map the result back. API keys
//! never pass through here: a profile only references a secret by id, and the factory reads the
//! value from the secret store at call time.
//!
//! These commands report failures as [`AiIpcError`], which has provider-specific codes
//! (`provider_auth`, `provider_rate_limited`, ...) next to the generic ones.

use std::error::Error;

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::ai::{AiUseCaseError, ProviderTestReport, UpsertProviderProfileInput};
use tan_application::ports::{requires_api_key, AiError, Capabilities};
use tan_domain::{
    ProviderKind, ProviderProfile, ProviderProfileId, SecretId, TaskAssignment, TaskKind,
};
use tan_infra_ai::defaults::{default_base_url, suggested_models};
use tauri::State;

use crate::state::AppState;

/// Which AI provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ProviderKindDto {
    /// `OpenAI`.
    #[serde(rename = "openai")]
    OpenAi,
    /// Anthropic.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Google Gemini.
    #[serde(rename = "gemini")]
    Gemini,
    /// `DeepSeek`.
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// A local or remote Ollama server.
    #[serde(rename = "ollama")]
    Ollama,
}

impl From<ProviderKind> for ProviderKindDto {
    fn from(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAi => Self::OpenAi,
            ProviderKind::Anthropic => Self::Anthropic,
            ProviderKind::Gemini => Self::Gemini,
            ProviderKind::DeepSeek => Self::DeepSeek,
            ProviderKind::Ollama => Self::Ollama,
        }
    }
}

impl From<ProviderKindDto> for ProviderKind {
    fn from(kind: ProviderKindDto) -> Self {
        match kind {
            ProviderKindDto::OpenAi => Self::OpenAi,
            ProviderKindDto::Anthropic => Self::Anthropic,
            ProviderKindDto::Gemini => Self::Gemini,
            ProviderKindDto::DeepSeek => Self::DeepSeek,
            ProviderKindDto::Ollama => Self::Ollama,
        }
    }
}

/// A job a provider profile can be assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindDto {
    /// Speech-to-text (unassigned: local whisper).
    Transcription,
    /// Screen-frame understanding.
    Vision,
    /// Note/summary generation.
    Summary,
}

impl From<TaskKind> for TaskKindDto {
    fn from(task: TaskKind) -> Self {
        match task {
            TaskKind::Transcription => Self::Transcription,
            TaskKind::Vision => Self::Vision,
            TaskKind::Summary => Self::Summary,
        }
    }
}

impl From<TaskKindDto> for TaskKind {
    fn from(task: TaskKindDto) -> Self {
        match task {
            TaskKindDto::Transcription => Self::Transcription,
            TaskKindDto::Vision => Self::Vision,
            TaskKindDto::Summary => Self::Summary,
        }
    }
}

/// What a provider can do.
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesDto {
    /// Text generation.
    pub chat: bool,
    /// Image understanding (for Ollama it depends on the model).
    pub vision: bool,
    /// Speech-to-text.
    pub transcription: bool,
}

impl From<Capabilities> for CapabilitiesDto {
    fn from(caps: Capabilities) -> Self {
        Self {
            chat: caps.chat,
            vision: caps.vision,
            transcription: caps.transcription,
        }
    }
}

/// A configured provider profile.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileDto {
    /// Identity.
    pub id: String,
    /// Which provider.
    pub kind: ProviderKindDto,
    /// Name shown in the UI.
    pub display_name: String,
    /// Model identifier.
    pub model: String,
    /// Custom endpoint; `null` uses the provider default.
    pub base_url: Option<String>,
    /// Id of the secret holding the API key.
    pub secret_ref: Option<String>,
    /// Whether the model understands images (the user's setting for Ollama; derived from the
    /// provider for the others).
    pub supports_vision: bool,
    /// What this profile can do.
    pub capabilities: CapabilitiesDto,
}

impl From<ProviderProfile> for ProviderProfileDto {
    fn from(profile: ProviderProfile) -> Self {
        let capabilities = Capabilities::for_profile(&profile);
        let ProviderProfile {
            id,
            kind,
            display_name,
            model,
            base_url,
            secret_ref,
            supports_vision,
        } = profile;
        Self {
            id: id.to_string(),
            kind: kind.into(),
            display_name,
            model,
            base_url,
            secret_ref: secret_ref.map(|s| s.to_string()),
            supports_vision,
            capabilities: capabilities.into(),
        }
    }
}

/// Input to create or update a provider profile.
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderProfileDto {
    /// Omit to create; set to update that profile.
    pub id: Option<String>,
    /// Which provider.
    pub kind: ProviderKindDto,
    /// Name shown in the UI.
    pub display_name: String,
    /// Model identifier.
    pub model: String,
    /// Custom endpoint root; omit for the provider default.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Id of the secret holding the API key (required except for Ollama).
    #[serde(default)]
    pub secret_ref: Option<String>,
    /// Whether the model understands images. Only Ollama profiles use it (default `false`).
    #[serde(default)]
    pub supports_vision: bool,
}

/// Which profile performs a task.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssignmentDto {
    /// The task.
    pub task: TaskKindDto,
    /// The profile that performs it.
    pub profile_id: String,
}

impl From<TaskAssignment> for TaskAssignmentDto {
    fn from(assignment: TaskAssignment) -> Self {
        Self {
            task: assignment.task.into(),
            profile_id: assignment.profile_id.to_string(),
        }
    }
}

/// Outcome of a successful connection test.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestReportDto {
    /// Round-trip time in milliseconds.
    pub latency_ms: u32,
    /// For Ollama: the models the server has.
    pub available_models: Vec<String>,
    /// For Ollama: whether the profile's model is installed; `null` for other providers.
    pub model_found: Option<bool>,
    /// For other providers: the start of the model's reply.
    pub reply_preview: Option<String>,
}

impl From<ProviderTestReport> for ProviderTestReportDto {
    fn from(report: ProviderTestReport) -> Self {
        Self {
            latency_ms: u32::try_from(report.latency_ms).unwrap_or(u32::MAX),
            available_models: report.available_models,
            model_found: report.model_found,
            reply_preview: report.reply_preview,
        }
    }
}

/// Suggested model identifiers of a provider (`null`: the provider cannot do that task).
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedModelsDto {
    /// For summaries.
    pub chat: Option<String>,
    /// For screen frames.
    pub vision: Option<String>,
    /// For speech-to-text.
    pub transcription: Option<String>,
}

/// Static facts about a provider kind, to pre-fill the profile form.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKindInfoDto {
    /// The provider.
    pub kind: ProviderKindDto,
    /// API root used when a profile sets no custom base URL.
    pub default_base_url: String,
    /// Whether a profile needs an API key secret.
    pub requires_api_key: bool,
    /// What the provider can do. For Ollama `vision` is `false` here: it is the profile's
    /// `supportsVision` flag that turns it on.
    pub capabilities: CapabilitiesDto,
    /// Model suggestions.
    pub suggested_models: SuggestedModelsDto,
}

/// Machine-readable failure class of the AI commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AiErrorCode {
    /// The input was rejected.
    Validation,
    /// The addressed entity does not exist.
    NotFound,
    /// A reference rule rejected the write.
    Conflict,
    /// The provider cannot perform the task.
    CapabilityMissing,
    /// The provider rejected the API key.
    ProviderAuth,
    /// The provider is rate limiting.
    ProviderRateLimited,
    /// The provider did not answer in time.
    ProviderTimeout,
    /// The provider refused the request.
    ProviderInvalidRequest,
    /// The provider does not offer what was asked.
    ProviderUnsupported,
    /// The provider could not be reached.
    ProviderNetwork,
    /// The provider answered with another error.
    ProviderError,
    /// The API key could not be read from the secret store (locked, missing...).
    SecretUnavailable,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned by the AI commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiIpcError {
    /// Failure class.
    pub code: AiErrorCode,
    /// Human-readable, key-free description (English; the UI localises by `code`).
    pub message: String,
}

impl From<AiUseCaseError> for AiIpcError {
    fn from(error: AiUseCaseError) -> Self {
        let code = match &error {
            AiUseCaseError::Invalid(_) => AiErrorCode::Validation,
            AiUseCaseError::NotFound { .. } => AiErrorCode::NotFound,
            AiUseCaseError::Conflict(_) => AiErrorCode::Conflict,
            AiUseCaseError::CapabilityMissing { .. } => AiErrorCode::CapabilityMissing,
            AiUseCaseError::Provider(provider) => match provider {
                AiError::Auth => AiErrorCode::ProviderAuth,
                AiError::RateLimited { .. } => AiErrorCode::ProviderRateLimited,
                AiError::Timeout => AiErrorCode::ProviderTimeout,
                AiError::InvalidRequest(_) => AiErrorCode::ProviderInvalidRequest,
                AiError::Unsupported(_) => AiErrorCode::ProviderUnsupported,
                AiError::Network(_) => AiErrorCode::ProviderNetwork,
                AiError::Provider { .. } => AiErrorCode::ProviderError,
                AiError::SecretUnavailable(_) => AiErrorCode::SecretUnavailable,
            },
            AiUseCaseError::Storage(_) => {
                log_source("AI operation failed", &error);
                AiErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
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

/// Lists the provider profiles.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_provider_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderProfileDto>, AiIpcError> {
    let profiles = state.list_provider_profiles.execute().await?;
    Ok(profiles.into_iter().map(ProviderProfileDto::from).collect())
}

/// Creates or updates a provider profile.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn upsert_provider_profile(
    state: State<'_, AppState>,
    input: UpsertProviderProfileDto,
) -> Result<ProviderProfileDto, AiIpcError> {
    let profile = state
        .upsert_provider_profile
        .execute(UpsertProviderProfileInput {
            id: input.id.map(ProviderProfileId::new),
            kind: input.kind.into(),
            display_name: input.display_name,
            model: input.model,
            base_url: input.base_url,
            secret_ref: input.secret_ref.map(SecretId::new),
            supports_vision: input.supports_vision,
        })
        .await?;
    Ok(profile.into())
}

/// Deletes a provider profile (and the task assignments pointing at it).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn delete_provider_profile(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AiIpcError> {
    state
        .delete_provider_profile
        .execute(&ProviderProfileId::new(id))
        .await?;
    Ok(())
}

/// Tests a stored profile: a short chat call, or for Ollama a model listing.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn test_provider(
    state: State<'_, AppState>,
    id: String,
) -> Result<ProviderTestReportDto, AiIpcError> {
    let report = state
        .test_provider
        .execute(&ProviderProfileId::new(id))
        .await?;
    Ok(report.into())
}

/// Static facts about every provider kind: default endpoint, capabilities, model suggestions.
#[tauri::command]
#[specta::specta]
#[must_use]
pub fn list_provider_kinds() -> Vec<ProviderKindInfoDto> {
    ProviderKind::ALL
        .iter()
        .map(|&kind| {
            let models = suggested_models(kind);
            ProviderKindInfoDto {
                kind: kind.into(),
                default_base_url: default_base_url(kind).to_owned(),
                requires_api_key: requires_api_key(kind),
                capabilities: Capabilities::for_provider(kind).into(),
                suggested_models: SuggestedModelsDto {
                    chat: models.chat.map(str::to_owned),
                    vision: models.vision.map(str::to_owned),
                    transcription: models.transcription.map(str::to_owned),
                },
            }
        })
        .collect()
}

/// Lists the task assignments.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn get_task_assignments(
    state: State<'_, AppState>,
) -> Result<Vec<TaskAssignmentDto>, AiIpcError> {
    let assignments = state.get_task_assignments.execute().await?;
    Ok(assignments
        .into_iter()
        .map(TaskAssignmentDto::from)
        .collect())
}

/// Assigns a task to a profile; refused when the provider lacks the capability.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn set_task_assignment(
    state: State<'_, AppState>,
    task: TaskKindDto,
    profile_id: String,
) -> Result<TaskAssignmentDto, AiIpcError> {
    let assignment = state
        .set_task_assignment
        .execute(task.into(), &ProviderProfileId::new(profile_id))
        .await?;
    Ok(assignment.into())
}

/// Removes a task's assignment (transcription then falls back to local whisper).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn clear_task_assignment(
    state: State<'_, AppState>,
    task: TaskKindDto,
) -> Result<(), AiIpcError> {
    state.clear_task_assignment.execute(task.into()).await?;
    Ok(())
}
