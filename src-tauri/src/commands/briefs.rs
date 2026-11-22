//! IPC commands for the brief of an action item: read it, generate it (again) and export it as
//! Markdown.
//!
//! Thin adapters over `ActionBriefService`. Generating calls the AI provider assigned to the
//! summary task and can take a minute; the command returns when the brief is stored. Failures
//! are reported as [`BriefIpcError`], whose provider codes mirror those of the AI commands.

use std::error::Error;

use serde::Serialize;
use specta::Type;
use tan_application::briefs::{ActionBriefView, BriefError};
use tan_application::ports::AiError;
use tan_domain::ActionItemId;
use tauri::State;

use crate::commands::processing::epoch_ms_to_f64;
use crate::commands::updates::{gated, StartKind, UpdateInProgress, UpdaterState};
use crate::state::AppState;

/// Machine-readable failure class of the action brief commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BriefErrorCode {
    /// The input was rejected.
    Validation,
    /// The action item (or, for the export, its brief) does not exist. It also means that the
    /// note was regenerated while the brief was being written: its action items are new.
    NotFound,
    /// A brief for this action item is already being generated.
    AlreadyRunning,
    /// No AI provider is assigned to the summary task.
    NoProvider,
    /// The model's reply was not a valid brief, even after a repair request.
    InvalidModelOutput,
    /// The session has no transcript or screen content to write a brief from.
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
    /// The CLI of a CLI provider is not installed or could not be started.
    CliUnavailable,
    /// An update is being installed (or waits for the restart): no generation can start until
    /// the application restarts.
    UpdateInProgress,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned by the action brief commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BriefIpcError {
    /// Failure class.
    pub code: BriefErrorCode,
    /// Human-readable, key-free description (English; the UI localises by `code`).
    pub message: String,
}

impl BriefIpcError {
    fn validation(message: &str) -> Self {
        Self {
            code: BriefErrorCode::Validation,
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

const fn provider_code(error: &AiError) -> BriefErrorCode {
    match error {
        AiError::Auth => BriefErrorCode::ProviderAuth,
        AiError::RateLimited { .. } => BriefErrorCode::ProviderRateLimited,
        AiError::Timeout => BriefErrorCode::ProviderTimeout,
        AiError::InvalidRequest(_) => BriefErrorCode::ProviderInvalidRequest,
        AiError::Unsupported(_) => BriefErrorCode::ProviderUnsupported,
        AiError::Network(_) => BriefErrorCode::ProviderNetwork,
        AiError::Provider { .. } => BriefErrorCode::ProviderError,
        AiError::SecretUnavailable(_) => BriefErrorCode::SecretUnavailable,
        AiError::CliUnavailable(_) => BriefErrorCode::CliUnavailable,
    }
}

impl From<BriefError> for BriefIpcError {
    fn from(error: BriefError) -> Self {
        let code = match &error {
            BriefError::NotFound(_) => BriefErrorCode::NotFound,
            BriefError::AlreadyRunning => BriefErrorCode::AlreadyRunning,
            BriefError::NoChatModel => BriefErrorCode::NoProvider,
            BriefError::Provider(inner) => provider_code(inner),
            BriefError::InvalidModelOutput(_) => BriefErrorCode::InvalidModelOutput,
            BriefError::NothingToSummarize => BriefErrorCode::NothingToSummarize,
            BriefError::Storage(_) | BriefError::Internal(_) => {
                log_source("action brief operation failed", &error);
                BriefErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

impl From<UpdateInProgress> for BriefIpcError {
    fn from(error: UpdateInProgress) -> Self {
        Self {
            code: BriefErrorCode::UpdateInProgress,
            message: error.to_string(),
        }
    }
}

/// A frame a brief points at.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BriefFrameDto {
    /// The frame (one of the session's frames).
    pub frame_id: String,
    /// Why it matters for the task.
    pub reason: String,
}

/// A moment of the conversation a brief quotes.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BriefQuoteDto {
    /// Offset into the meeting, in milliseconds (whole seconds).
    pub at_ms: u32,
    /// Who spoke: `me`, `others` or a name.
    pub speaker: String,
    /// The words, in the spoken language.
    pub text: String,
}

/// The brief of an action item. It is read-only: the user can regenerate it, never edit it.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActionBriefDto {
    /// The action item the brief is about.
    pub action_item_id: String,
    /// The language it was written in (the note's).
    pub language: String,
    /// Why the task exists and what it is about.
    pub context: String,
    /// What the meeting said about how, with whom, by when or under which constraints.
    pub details: Vec<String>,
    /// The note's decisions that bear on the task.
    pub related_decisions: Vec<String>,
    /// The note's open questions that bear on the task.
    pub open_questions: Vec<String>,
    /// Frames that matter for the task.
    pub frames: Vec<BriefFrameDto>,
    /// Quotes with their time stamps, in time order.
    pub quotes: Vec<BriefQuoteDto>,
    /// Steps the AI suggests. Nobody agreed to them in the meeting: show them as suggestions.
    pub suggested_steps: Vec<String>,
    /// The provider profile whose model wrote it; `null` when that profile was deleted since.
    pub provider_profile_id: Option<String>,
    /// When it was generated, milliseconds since the Unix epoch.
    pub created_at: f64,
}

impl From<ActionBriefView> for ActionBriefDto {
    fn from(view: ActionBriefView) -> Self {
        let content = view.content;
        Self {
            action_item_id: view.action_item_id.to_string(),
            language: view.language.to_string(),
            context: content.context,
            details: content.details,
            related_decisions: content.related_decisions,
            open_questions: content.open_questions,
            frames: content
                .frames
                .into_iter()
                .map(|frame| BriefFrameDto {
                    frame_id: frame.frame_id.to_string(),
                    reason: frame.reason,
                })
                .collect(),
            quotes: content
                .quotes
                .into_iter()
                .map(|quote| BriefQuoteDto {
                    at_ms: u32::try_from(quote.at_ms).unwrap_or(u32::MAX),
                    speaker: quote.speaker,
                    text: quote.text,
                })
                .collect(),
            suggested_steps: content.suggested_steps,
            provider_profile_id: view.provider_profile_id.map(|id| id.to_string()),
            created_at: epoch_ms_to_f64(view.created_at),
        }
    }
}

fn parse_action_item_id(action_item_id: String) -> Result<ActionItemId, BriefIpcError> {
    if action_item_id.trim().is_empty() {
        return Err(BriefIpcError::validation(
            "invalid input: the action item id is empty",
        ));
    }
    Ok(ActionItemId::new(action_item_id))
}

/// The stored brief of an action item, or `null` when none was generated yet. An action item
/// that does not exist is a `not_found` error.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn get_action_brief(
    state: State<'_, AppState>,
    action_item_id: String,
) -> Result<Option<ActionBriefDto>, BriefIpcError> {
    let id = parse_action_item_id(action_item_id)?;
    Ok(state.action_briefs.get(&id).await?.map(Into::into))
}

/// Writes the brief of an action item with the AI provider assigned to the summary task, in the
/// language of the note, stores it (replacing an earlier brief) and returns it. The call returns
/// when the brief is stored; it can take a minute for a long meeting.
///
/// It works while the session is still recording or being processed, from what exists at that
/// moment. A second call for the same action item while one is running is refused
/// (`already_running`); without an assigned provider the error is `no_provider`.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn generate_action_brief(
    state: State<'_, AppState>,
    updater_state: State<'_, UpdaterState>,
    action_item_id: String,
) -> Result<ActionBriefDto, BriefIpcError> {
    // Refused while an update installs; the permit is held for the whole generation, so the
    // install sees it as active.
    gated(
        &updater_state,
        StartKind::Processing,
        Box::pin(async {
            let id = parse_action_item_id(action_item_id)?;
            Ok(state.action_briefs.generate(&id).await?.into())
        }),
    )
    .await
}

/// The stored brief as Markdown text, in the language it was written in, for the UI to copy or
/// save. The suggested steps sit under the heading "Öneri" ("Suggestion" in English). An action
/// item without a brief is a `not_found` error.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn export_action_brief_markdown(
    state: State<'_, AppState>,
    action_item_id: String,
) -> Result<String, BriefIpcError> {
    let id = parse_action_item_id(action_item_id)?;
    Ok(state.action_briefs.export_markdown(&id).await?)
}
