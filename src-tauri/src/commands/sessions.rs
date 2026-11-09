//! IPC commands for browsing sessions: list, search, detail, action items, Markdown and audio
//! export, disk usage and deletion.
//!
//! Thin adapters over the session use cases; the DTOs carry only what the UI shows.

use std::error::Error;

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::ports::{SearchHit, SearchSource};
use tan_application::sessions::{
    resolve_export_destination_as, AudioTrackState, ExportDestination, ExportDestinationProblem,
    ExportExtension, NoteWithItems, Page, SessionDetail, SessionFilter, SessionListItem,
    SessionPage, SessionSearchResult, SessionsError, StorageUsage,
};
use tan_domain::{
    ActionItem, ActionItemId, EpochMs, Frame, Session, SessionId, SessionStatus, TranscriptSegment,
};
use tauri::{AppHandle, Manager, State};

use crate::commands::processing::{epoch_ms_to_f64, ProcessingJobDto};
use crate::state::AppState;

/// Machine-readable failure class of the session commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SessionsErrorCode {
    /// The input was rejected.
    Validation,
    /// The session (or action item) does not exist.
    NotFound,
    /// The session is being recorded or processed and cannot be changed now.
    Busy,
    /// A file operation failed; details are in the log.
    FilesFailed,
    /// Anything else; details are in the log.
    Internal,
}

/// Why an export command refused (set with `code: validation`): the `destination` of
/// `export_session_markdown` / `export_session_audio`, or, for the audio export, the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportProblemCode {
    /// The path is not absolute.
    NotAbsolute,
    /// The path contains `..`.
    ParentReference,
    /// The path is a UNC, network or device path.
    NetworkPath,
    /// The path has no file name.
    MissingName,
    /// The file has an extension other than the one the export writes (`.md` for the note,
    /// `.wav` for the audio).
    InvalidExtension,
    /// The path is not the one the user picked in the save dialog.
    NotDialogSelected,
    /// The path is inside Tan's own data folder.
    InsideDataDir,
    /// The folder the file should go into does not exist.
    FolderMissing,
    /// The export's extension was appended to the picked name and that file already exists.
    AlreadyExists,
    /// `export_session_audio` only: the session has no audio to export (none recorded, deleted
    /// by the retention policy, or the files are gone).
    NoAudio,
    /// `export_session_audio` only: the session is still being recorded.
    SessionRecording,
}

impl From<ExportDestinationProblem> for ExportProblemCode {
    fn from(problem: ExportDestinationProblem) -> Self {
        match problem {
            ExportDestinationProblem::NotAbsolute => Self::NotAbsolute,
            ExportDestinationProblem::ParentReference => Self::ParentReference,
            ExportDestinationProblem::NetworkPath => Self::NetworkPath,
            ExportDestinationProblem::MissingName => Self::MissingName,
            ExportDestinationProblem::InvalidExtension
            | ExportDestinationProblem::InvalidAudioExtension => Self::InvalidExtension,
            ExportDestinationProblem::NotDialogSelected => Self::NotDialogSelected,
            ExportDestinationProblem::InsideDataDir => Self::InsideDataDir,
            ExportDestinationProblem::FolderMissing => Self::FolderMissing,
            ExportDestinationProblem::AlreadyExists => Self::AlreadyExists,
        }
    }
}

/// Error returned by the session commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionsIpcError {
    /// Failure class.
    pub code: SessionsErrorCode,
    /// Human-readable description (English; the UI localises by `code`).
    pub message: String,
    /// Set by the export commands when they were refused for a typed reason (a bad destination,
    /// or for the audio export a session without audio or still recording): the exact reason,
    /// so the UI never parses `message`. `None` for every other failure.
    pub export_problem: Option<ExportProblemCode>,
}

impl SessionsIpcError {
    fn validation(message: &str) -> Self {
        Self {
            code: SessionsErrorCode::Validation,
            message: message.to_owned(),
            export_problem: None,
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

impl From<SessionsError> for SessionsIpcError {
    fn from(error: SessionsError) -> Self {
        let export_problem = match &error {
            SessionsError::ExportDestination(problem) => Some((*problem).into()),
            SessionsError::NoAudio => Some(ExportProblemCode::NoAudio),
            SessionsError::Recording => Some(ExportProblemCode::SessionRecording),
            _ => None,
        };
        let code = match &error {
            SessionsError::NotFound(_) => SessionsErrorCode::NotFound,
            SessionsError::Invalid(_)
            | SessionsError::ExportDestination(_)
            | SessionsError::NoAudio
            | SessionsError::Recording => SessionsErrorCode::Validation,
            SessionsError::Busy(_) => SessionsErrorCode::Busy,
            SessionsError::Files(_) => {
                log_source("session file operation failed", &error);
                SessionsErrorCode::FilesFailed
            }
            SessionsError::Storage(_) => {
                log_source("session operation failed", &error);
                SessionsErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
            export_problem,
        }
    }
}

impl From<ExportDestinationProblem> for SessionsIpcError {
    fn from(problem: ExportDestinationProblem) -> Self {
        Self::from(SessionsError::from(problem))
    }
}

// ---------------------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------------------

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Where a session is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusDto {
    /// Capture is running.
    Recording,
    /// Capture finished; nothing processed yet.
    Recorded,
    /// The pipeline is running.
    Processing,
    /// The pipeline finished.
    Processed,
    /// Capture or processing failed; processing can be retried.
    Failed,
}

impl From<SessionStatus> for SessionStatusDto {
    fn from(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Recording => Self::Recording,
            SessionStatus::Recorded => Self::Recorded,
            SessionStatus::Processing => Self::Processing,
            SessionStatus::Processed => Self::Processed,
            SessionStatus::Failed => Self::Failed,
        }
    }
}

impl From<SessionStatusDto> for SessionStatus {
    fn from(status: SessionStatusDto) -> Self {
        match status {
            SessionStatusDto::Recording => Self::Recording,
            SessionStatusDto::Recorded => Self::Recorded,
            SessionStatusDto::Processing => Self::Processing,
            SessionStatusDto::Processed => Self::Processed,
            SessionStatusDto::Failed => Self::Failed,
        }
    }
}

/// A session as the UI lists it.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    /// Identity.
    pub id: String,
    /// Title.
    pub title: String,
    /// What produced the session (`meeting`).
    pub kind: String,
    /// Lifecycle status.
    pub status: SessionStatusDto,
    /// When capture started, milliseconds since the Unix epoch.
    pub started_at: f64,
    /// When capture ended; `null` while recording.
    pub ended_at: Option<f64>,
    /// Length in milliseconds once capture has ended.
    pub duration_ms: Option<u32>,
    /// Spoken language: `auto` or a language code.
    pub language: String,
    /// Language the note is generated in.
    pub note_language: String,
}

impl From<&Session> for SessionDto {
    fn from(session: &Session) -> Self {
        Self {
            id: session.id.to_string(),
            title: session.title.clone(),
            kind: session.kind.as_str().to_owned(),
            status: session.status().into(),
            started_at: epoch_ms_to_f64(session.started_at),
            ended_at: session.ended_at().map(epoch_ms_to_f64),
            duration_ms: session.duration_ms().map(saturating_u32),
            language: session.language.to_storage_string(),
            note_language: session.note_language.to_string(),
        }
    }
}

/// A session row of the list.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionListItemDto {
    /// The session.
    pub session: SessionDto,
    /// Whether a note has been generated.
    pub has_note: bool,
    /// Action items in the note.
    pub action_items_total: u32,
    /// Of those, the ones marked done.
    pub action_items_done: u32,
}

impl From<&SessionListItem> for SessionListItemDto {
    fn from(item: &SessionListItem) -> Self {
        Self {
            session: (&item.session).into(),
            has_note: item.has_note,
            action_items_total: count_u32(item.action_items_total),
            action_items_done: count_u32(item.action_items_done),
        }
    }
}

/// One page of the session list.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionPageDto {
    /// The sessions of this page, newest first.
    pub items: Vec<SessionListItemDto>,
    /// Sessions matching the filter across all pages.
    pub total: u32,
    /// The offset that was applied.
    pub offset: u32,
    /// The page size that was applied (at most 200).
    pub limit: u32,
}

impl From<&SessionPage> for SessionPageDto {
    fn from(page: &SessionPage) -> Self {
        Self {
            items: page.items.iter().map(Into::into).collect(),
            total: count_u32(page.total),
            offset: count_u32(page.offset),
            limit: count_u32(page.limit),
        }
    }
}

/// Which sessions to list; every set field must match.
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionFilterDto {
    /// Only sessions in this status.
    pub status: Option<SessionStatusDto>,
    /// Only sessions whose title contains this text (case-insensitive).
    pub title_contains: Option<String>,
    /// Only sessions that started at or after this time (milliseconds since the Unix epoch).
    pub started_from: Option<f64>,
    /// Only sessions that started before this time.
    pub started_before: Option<f64>,
}

/// Which page of the list to return.
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct PageDto {
    /// Sessions to skip (default 0).
    pub offset: Option<u32>,
    /// Sessions to return (default 50, at most 200).
    pub limit: Option<u32>,
}

/// JS numbers are doubles; a timestamp that is not finite is a validation error.
fn epoch_ms_from_f64(value: f64, field: &str) -> Result<EpochMs, SessionsIpcError> {
    if !value.is_finite() {
        return Err(SessionsIpcError::validation(&format!(
            "invalid input: {field} is not a valid time"
        )));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(EpochMs::new(value as i64))
}

impl SessionFilterDto {
    fn into_filter(self) -> Result<SessionFilter, SessionsIpcError> {
        Ok(SessionFilter {
            status: self.status.map(Into::into),
            title_contains: self.title_contains,
            started_from: self
                .started_from
                .map(|v| epoch_ms_from_f64(v, "startedFrom"))
                .transpose()?,
            started_before: self
                .started_before
                .map(|v| epoch_ms_from_f64(v, "startedBefore"))
                .transpose()?,
        })
    }
}

/// Where a search hit was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SearchSourceDto {
    /// A transcript segment.
    Transcript,
    /// The note (summary, decisions, open questions).
    Note,
}

/// One search hit.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitDto {
    /// What matched.
    pub source: SearchSourceDto,
    /// For transcript hits: where in the meeting, in milliseconds.
    pub start_ms: Option<u32>,
    /// The beginning of the matching text.
    pub excerpt: String,
}

impl From<&SearchHit> for SearchHitDto {
    fn from(hit: &SearchHit) -> Self {
        Self {
            source: match hit.source {
                SearchSource::Transcript => SearchSourceDto::Transcript,
                SearchSource::Note => SearchSourceDto::Note,
            },
            start_ms: hit.start_ms.map(saturating_u32),
            excerpt: hit.excerpt.clone(),
        }
    }
}

/// A session with the hits that matched a search.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResultDto {
    /// The session.
    pub session: SessionDto,
    /// Its matches, best first.
    pub hits: Vec<SearchHitDto>,
}

impl From<&SessionSearchResult> for SessionSearchResultDto {
    fn from(result: &SessionSearchResult) -> Self {
        Self {
            session: (&result.session).into(),
            hits: result.hits.iter().map(Into::into).collect(),
        }
    }
}

/// An audio track of a session.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrackDto {
    /// `microphone` or `system`.
    pub kind: String,
    /// Path relative to the session folder.
    pub path: String,
    /// Samples per second.
    pub sample_rate: u32,
    /// Length in milliseconds.
    pub duration_ms: u32,
    /// When the retention policy deleted the file; `null` while it is kept.
    pub deleted_at: Option<f64>,
}

impl From<&AudioTrackState> for AudioTrackDto {
    fn from(state: &AudioTrackState) -> Self {
        Self {
            kind: state.track.kind.as_str().to_owned(),
            path: state.track.path.clone(),
            sample_rate: state.track.sample_rate,
            duration_ms: saturating_u32(state.track.duration_ms),
            deleted_at: state.deleted_at.map(epoch_ms_to_f64),
        }
    }
}

/// A stored screen frame.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FrameDto {
    /// Identity (note key frames refer to it).
    pub id: String,
    /// Offset from the session start, in milliseconds.
    pub timestamp_ms: u32,
    /// Image path relative to the session folder (`sessionDir`).
    pub path: String,
    /// The vision model's description, when it ran.
    pub description: Option<String>,
}

impl From<&Frame> for FrameDto {
    fn from(frame: &Frame) -> Self {
        Self {
            id: frame.id.to_string(),
            timestamp_ms: saturating_u32(frame.timestamp_ms),
            path: frame.path.clone(),
            description: frame.description.clone(),
        }
    }
}

/// One transcribed utterance.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLineDto {
    /// `microphone` or `system`.
    pub track: String,
    /// Start offset in milliseconds.
    pub start_ms: u32,
    /// End offset in milliseconds.
    pub end_ms: u32,
    /// Spoken language, when known.
    pub language: Option<String>,
    /// The text, in the spoken language.
    pub text: String,
    /// `me`, `others` (the UI localises them) or another label.
    pub speaker: Option<String>,
}

impl From<&TranscriptSegment> for TranscriptLineDto {
    fn from(segment: &TranscriptSegment) -> Self {
        Self {
            track: segment.track.as_str().to_owned(),
            start_ms: saturating_u32(segment.start_ms),
            end_ms: saturating_u32(segment.end_ms),
            language: segment.language.as_ref().map(ToString::to_string),
            text: segment.text.clone(),
            speaker: segment.speaker.clone(),
        }
    }
}

/// A task from the note.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActionItemDto {
    /// Identity (pass it to `toggle_action_item`).
    pub id: String,
    /// What has to be done.
    pub task: String,
    /// Who is responsible, when known.
    pub owner: Option<String>,
    /// Due date at midnight UTC (milliseconds since the Unix epoch), when known.
    pub due: Option<f64>,
    /// Whether the user marked it done.
    pub done: bool,
}

impl From<&ActionItem> for ActionItemDto {
    fn from(item: &ActionItem) -> Self {
        Self {
            id: item.id.to_string(),
            task: item.task.clone(),
            owner: item.owner.clone(),
            due: item.due.map(epoch_ms_to_f64),
            done: item.done,
        }
    }
}

/// The generated note.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct NoteDto {
    /// Identity.
    pub id: String,
    /// Language the note was generated in.
    pub language: String,
    /// The summary.
    pub summary: String,
    /// Decisions taken.
    pub decisions: Vec<String>,
    /// Questions left open.
    pub open_questions: Vec<String>,
    /// Ids of the key frames, in order (see `frames` of the detail).
    pub key_frame_ids: Vec<String>,
    /// When it was generated.
    pub created_at: f64,
    /// The tasks.
    pub action_items: Vec<ActionItemDto>,
}

impl From<&NoteWithItems> for NoteDto {
    fn from(note: &NoteWithItems) -> Self {
        Self {
            id: note.note.id.to_string(),
            language: note.note.language.to_string(),
            summary: note.note.summary.clone(),
            decisions: note.note.decisions.clone(),
            open_questions: note.note.open_questions.clone(),
            key_frame_ids: note
                .note
                .frame_refs
                .iter()
                .map(ToString::to_string)
                .collect(),
            created_at: epoch_ms_to_f64(note.note.created_at),
            action_items: note.action_items.iter().map(Into::into).collect(),
        }
    }
}

/// Everything the session screen shows.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetailDto {
    /// The session.
    pub session: SessionDto,
    /// Absolute path of the session folder; frame and audio paths are relative to it.
    pub session_dir: String,
    /// Its audio tracks.
    pub audio_tracks: Vec<AudioTrackDto>,
    /// Its frames in time order.
    pub frames: Vec<FrameDto>,
    /// The transcript in time order.
    pub transcript: Vec<TranscriptLineDto>,
    /// The latest note, if one was generated.
    pub note: Option<NoteDto>,
    /// The processing job, if the session was ever processed.
    pub job: Option<ProcessingJobDto>,
}

impl SessionDetailDto {
    fn new(detail: &SessionDetail, session_dir: String) -> Self {
        Self {
            session: (&detail.session).into(),
            session_dir,
            audio_tracks: detail.audio_tracks.iter().map(Into::into).collect(),
            frames: detail.frames.iter().map(Into::into).collect(),
            transcript: detail.transcript.iter().map(Into::into).collect(),
            note: detail.note.as_ref().map(Into::into),
            job: detail.job.as_ref().map(Into::into),
        }
    }
}

/// Disk space used by Tan's data.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsageDto {
    /// Everything under the data directory, in bytes.
    pub total_bytes: f64,
    /// Audio files.
    pub audio_bytes: f64,
    /// Frame images.
    pub frame_bytes: f64,
    /// Downloaded whisper models.
    pub model_bytes: f64,
}

impl From<StorageUsage> for StorageUsageDto {
    #[allow(clippy::cast_precision_loss)] // JS numbers are doubles; exact up to 8 PB
    fn from(usage: StorageUsage) -> Self {
        Self {
            total_bytes: usage.total_bytes as f64,
            audio_bytes: usage.audio_bytes as f64,
            frame_bytes: usage.frame_bytes as f64,
            model_bytes: usage.model_bytes as f64,
        }
    }
}

fn parse_session_id(session_id: String) -> Result<SessionId, SessionsIpcError> {
    if session_id.trim().is_empty() {
        return Err(SessionsIpcError::validation(
            "invalid input: the session id is empty",
        ));
    }
    Ok(SessionId::new(session_id))
}

// ---------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------

/// Lists sessions, newest first, with an optional filter and page.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_sessions(
    state: State<'_, AppState>,
    filter: Option<SessionFilterDto>,
    page: Option<PageDto>,
) -> Result<SessionPageDto, SessionsIpcError> {
    let filter = filter.unwrap_or_default().into_filter()?;
    let page = page.unwrap_or_default();
    let page = Page {
        offset: usize::try_from(page.offset.unwrap_or(0)).unwrap_or(usize::MAX),
        limit: page
            .limit
            .map_or(tan_application::sessions::DEFAULT_PAGE_SIZE, |l| {
                usize::try_from(l).unwrap_or(usize::MAX)
            }),
    };
    let result = state.list_sessions.execute(&filter, page).await?;
    Ok((&result).into())
}

/// Full-text search over transcripts and notes (every word must match; prefixes count),
/// grouped by session.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn search_sessions(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<SessionSearchResultDto>, SessionsIpcError> {
    let results = state.search_sessions.execute(&query).await?;
    Ok(results.iter().map(Into::into).collect())
}

/// Lets the webview read `dir` (a folder of one session) through the asset protocol. Failing
/// only costs the images or the player, so it is logged, not returned.
fn allow_in_asset_scope(app: &AppHandle, dir: std::io::Result<std::path::PathBuf>, what: &str) {
    match dir {
        Ok(dir) => {
            if let Err(error) = app.asset_protocol_scope().allow_directory(&dir, true) {
                tracing::warn!(%error, folder = what, "cannot allow the folder in the asset scope");
            }
        }
        Err(error) => tracing::warn!(%error, folder = what, "no such folder for the session"),
    }
}

/// A session with its audio tracks, frames, transcript, note and processing job.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn get_session_detail(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionDetailDto, SessionsIpcError> {
    let id = parse_session_id(session_id)?;
    let detail = state.get_session_detail.execute(&id).await?;
    // Frame images and audio are shown and played through the asset protocol, which can read
    // nothing until its scope allows it. The scope grows by the `frames/` and `audio/` folders
    // of each session whose detail is requested and keeps them until the app exits (never the
    // database, `notes.md` or the rest of the session folder). The save dialog also adds the
    // export file the user picks. So the real bound is: the frames and audio folders of the
    // sessions opened in this run (any session's, since the webview can request any id) plus
    // the dialog-picked export file.
    allow_in_asset_scope(&app, state.layout.frames_dir(&id), "frames");
    allow_in_asset_scope(&app, state.layout.audio_dir(&id), "audio");
    let session_dir = state
        .layout
        .session_dir(&id)
        .map(|dir| dir.display().to_string())
        .map_err(|e| SessionsIpcError::validation(&format!("invalid input: {e}")))?;
    Ok(SessionDetailDto::new(&detail, session_dir))
}

/// Marks an action item done or not done.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn toggle_action_item(
    state: State<'_, AppState>,
    action_item_id: String,
    done: bool,
) -> Result<(), SessionsIpcError> {
    if action_item_id.trim().is_empty() {
        return Err(SessionsIpcError::validation(
            "invalid input: the action item id is empty",
        ));
    }
    Ok(state
        .toggle_action_item
        .execute(&ActionItemId::new(action_item_id), done)
        .await?)
}

/// The checks every export to a path the user picked in the save dialog goes through, in this
/// order: the shape of the path, the dialog's pick, the data folder, and an extension that
/// was added to the picked name.
fn check_picked_destination(
    app: &AppHandle,
    state: &AppState,
    destination: &str,
    extension: ExportExtension,
) -> Result<ExportDestination, SessionsIpcError> {
    let chosen = std::path::Path::new(destination);
    // 1. Shape of the path first, without touching the file system: the scope check below
    //    stats the path, which for a UNC path would open a network connection.
    let target = resolve_export_destination_as(chosen, extension)?;
    // 2. The path must be exactly the one the user picked in the save dialog (the dialog
    //    plugin adds it to the asset scope; the UI sends it back unchanged).
    if !app.asset_protocol_scope().is_allowed(chosen) {
        return Err(ExportDestinationProblem::NotDialogSelected.into());
    }
    // 3. The asset scope also holds the frames and audio folders of opened sessions: only a
    //    place outside Tan's own data folder counts as a dialog pick.
    match state.layout.contains(&target.path) {
        Ok(false) => {}
        Ok(true) => return Err(ExportDestinationProblem::InsideDataDir.into()),
        Err(_) => return Err(ExportDestinationProblem::FolderMissing.into()),
    }
    // 4. The dialog confirmed replacing the path as picked. When the extension was added, the
    //    file now named may be a different, existing one nobody confirmed: refuse.
    if target.extension_added && target.path.exists() {
        return Err(ExportDestinationProblem::AlreadyExists.into());
    }
    Ok(target)
}

/// Exports the session's note, key frames and transcript as a Markdown file and returns the
/// file's absolute path.
///
/// With `destination` (the absolute `.md` path the user picked in the save dialog) the file is
/// written there and the key frames are copied into a `<name>_frames` folder beside it, with
/// relative links. `destination` is the path the save dialog returned, sent back unchanged:
/// it must be absolute, on a local drive (UNC and device paths are refused), outside Tan's
/// data folder and exactly what the user picked in the dialog. A path without an extension
/// gets `.md` (a file already having that name is not replaced); any other extension is
/// refused. Every refusal is a `validation` error whose `exportProblem` field holds the exact
/// reason (`not_absolute`, `parent_reference`, `network_path`, `missing_name`,
/// `invalid_extension`, `not_dialog_selected`, `inside_data_dir`, `folder_missing`,
/// `already_exists`). Without it the file goes into the session folder (frame links relative to it).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn export_session_markdown(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    destination: Option<String>,
) -> Result<String, SessionsIpcError> {
    let id = parse_session_id(session_id)?;
    let detail = state.get_session_detail.execute(&id).await?;
    let path = match destination {
        Some(destination) => {
            let target =
                check_picked_destination(&app, &state, &destination, ExportExtension::Markdown)?;
            // Copies images and writes files: keep it off the async worker.
            let export = state.export_session.clone();
            tauri::async_runtime::spawn_blocking(move || export.execute_to(&detail, &target.path))
                .await
                .map_err(|e| SessionsIpcError {
                    code: SessionsErrorCode::Internal,
                    message: format!("the export did not finish: {e}"),
                    export_problem: None,
                })??
        }
        None => state.export_session.execute(&detail)?,
    };
    Ok(path.display().to_string())
}

/// Mixes the session's audio tracks (microphone and system sound) into one mono WAV file and
/// returns the file's absolute path.
///
/// `destination` is the absolute `.wav` path the user picked in the save dialog, sent back
/// unchanged; the rules are those of `export_session_markdown`'s `destination` (absolute, local
/// drive, outside Tan's data folder, exactly what the dialog returned; a path without an
/// extension gets `.wav` and a file already having that name is not replaced; any other
/// extension is refused), and a refusal is the same `validation` error with the reason in
/// `exportProblem`. Also `validation`: `exportProblem: no_audio` when the session has no audio
/// left to export (none recorded, deleted by the retention policy, files gone) and
/// `exportProblem: session_recording` while the session is still being recorded. Tracks of a
/// different sample rate are brought to the higher one and the shorter is completed with
/// silence. A file already at `destination` is replaced; a failure leaves no partial file.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn export_session_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    destination: String,
) -> Result<String, SessionsIpcError> {
    let id = parse_session_id(session_id)?;
    let detail = state.get_session_detail.execute(&id).await?;
    let target = check_picked_destination(&app, &state, &destination, ExportExtension::Wav)?;
    // Reads, resamples and writes the whole recording: keep it off the async worker.
    let export = state.export_audio.clone();
    let path =
        tauri::async_runtime::spawn_blocking(move || export.execute_to(&detail, &target.path))
            .await
            .map_err(|e| SessionsIpcError {
                code: SessionsErrorCode::Internal,
                message: format!("the export did not finish: {e}"),
                export_problem: None,
            })??;
    Ok(path.display().to_string())
}

/// How much disk Tan's data uses: total, audio, frames and whisper models.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn get_storage_usage(
    state: State<'_, AppState>,
) -> Result<StorageUsageDto, SessionsIpcError> {
    // Walks the data directory: keep it off the async worker.
    let usage = {
        let measure = state.storage_usage.clone();
        tauri::async_runtime::spawn_blocking(move || measure.execute())
            .await
            .map_err(|e| SessionsIpcError {
                code: SessionsErrorCode::Internal,
                message: format!("the measurement did not finish: {e}"),
                export_problem: None,
            })??
    };
    Ok(usage.into())
}

/// Deletes a session with its files, transcript, frames, note and action items. Irreversible,
/// so the caller must pass `confirm: true` (after asking the user); a session that is being
/// recorded or processed is refused.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn delete_session(
    state: State<'_, AppState>,
    session_id: String,
    confirm: bool,
) -> Result<(), SessionsIpcError> {
    let id = parse_session_id(session_id)?;
    if !confirm {
        return Err(SessionsIpcError::validation(
            "invalid input: deleting a session must be confirmed",
        ));
    }
    Ok(state.delete_session.execute(&id).await?)
}
