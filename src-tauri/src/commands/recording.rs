//! IPC commands and events for recording.
//!
//! Thin adapters over `RecordingService`. Recording keeps running when the window is closed
//! (the tray stays), so everything the UI needs to catch up is available again through
//! [`recording_status`] and the events below.

use std::error::Error;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::ports::{
    AudioCapture, CaptureErrorKind, Clock, ScreenAvailability, ScreenCapture,
    ScreenUnavailableReason,
};
use tan_application::recording::{
    default_session_title, RecordingError, RecordingEvent, RecordingEvents, RecordingStatus,
    RecordingSummary, StartOptions, TrackSummary,
};
use tan_domain::{LanguageCode, LanguageSetting, SessionStatus};
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;

use crate::state::{AppState, SystemClock};
use crate::tray;

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

/// Machine-readable failure class of the recording commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RecordingErrorCode {
    /// The input was rejected.
    Validation,
    /// A recording is already running.
    AlreadyRecording,
    /// No recording is running.
    NotRecording,
    /// The OS denied microphone or screen access; the message says which setting to change.
    PermissionDenied,
    /// A requested or default device does not exist.
    DeviceNotFound,
    /// This platform cannot capture that source.
    CaptureUnsupported,
    /// The capture backend failed.
    CaptureFailed,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned by the recording commands.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordingIpcError {
    /// Failure class.
    pub code: RecordingErrorCode,
    /// Human-readable description (English; the UI localises by `code`).
    pub message: String,
}

impl RecordingIpcError {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            code: RecordingErrorCode::Validation,
            message: message.into(),
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

impl From<RecordingError> for RecordingIpcError {
    fn from(error: RecordingError) -> Self {
        let code = match &error {
            RecordingError::Invalid(_) => RecordingErrorCode::Validation,
            RecordingError::AlreadyRecording => RecordingErrorCode::AlreadyRecording,
            RecordingError::NotRecording => RecordingErrorCode::NotRecording,
            RecordingError::Capture(capture) => match capture.kind {
                CaptureErrorKind::PermissionDenied => RecordingErrorCode::PermissionDenied,
                CaptureErrorKind::DeviceNotFound => RecordingErrorCode::DeviceNotFound,
                CaptureErrorKind::Unsupported => RecordingErrorCode::CaptureUnsupported,
                CaptureErrorKind::Failed => RecordingErrorCode::CaptureFailed,
            },
            RecordingError::Storage(_) | RecordingError::Io(_) => {
                log_source("recording operation failed", &error);
                RecordingErrorCode::Internal
            }
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------------------

/// How to start a recording. Every field is optional: missing values fall back to the
/// settings (languages), the system default (devices) and a generated title.
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct StartRecordingOptionsDto {
    /// Title of the new session.
    pub title: Option<String>,
    /// Spoken language: `auto` or a language code.
    pub speech_language: Option<String>,
    /// Language the notes are generated in.
    pub note_language: Option<String>,
    /// Microphone to record (an id from `list_audio_devices`).
    pub microphone_device_id: Option<String>,
    /// Display to sample (an id from `list_displays`).
    pub display_id: Option<String>,
}

impl StartRecordingOptionsDto {
    fn into_options(self, clock: &dyn Clock) -> Result<StartOptions, RecordingIpcError> {
        let title = self
            .title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| default_session_title(clock.now()));
        let speech_language = self
            .speech_language
            .as_deref()
            .map(str::parse::<LanguageSetting>)
            .transpose()
            .map_err(|e| RecordingIpcError::validation(e.to_string()))?;
        let note_language = self
            .note_language
            .as_deref()
            .map(LanguageCode::new)
            .transpose()
            .map_err(|e| RecordingIpcError::validation(e.to_string()))?;
        Ok(StartOptions {
            title,
            speech_language,
            note_language,
            microphone_device_id: self.microphone_device_id.filter(|id| !id.is_empty()),
            display_id: self.display_id.filter(|id| !id.is_empty()),
        })
    }
}

/// A recording that has just started.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStartedDto {
    /// The new session.
    pub session_id: String,
    /// Its title.
    pub title: String,
    /// Start time, milliseconds since the Unix epoch.
    pub started_at: f64,
}

/// One audio track of a finished recording.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummaryDto {
    /// `microphone` or `system`.
    pub kind: String,
    /// Length of the audio file in milliseconds.
    pub duration_ms: u32,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Highest sample magnitude seen (`0` = the source delivered only digital silence).
    pub peak: f32,
    /// How late the source started delivering audio, in milliseconds.
    pub first_chunk_offset_ms: u32,
    /// Silence inserted to keep the track on the shared timeline, in milliseconds.
    pub silence_inserted_ms: u32,
    /// Audio blocks dropped because the writer could not keep up.
    pub dropped_chunks: u32,
    /// Problems the capture backend reported (xruns, device changes).
    pub stream_issues: u32,
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl From<TrackSummary> for TrackSummaryDto {
    fn from(track: TrackSummary) -> Self {
        Self {
            kind: track.kind.as_str().to_owned(),
            duration_ms: saturating_u32(track.duration_ms),
            sample_rate: track.sample_rate,
            channels: track.channels,
            peak: track.peak,
            first_chunk_offset_ms: saturating_u32(track.first_chunk_offset_ms),
            silence_inserted_ms: saturating_u32(track.silence_inserted_ms),
            dropped_chunks: saturating_u32(track.dropped_chunks),
            stream_issues: track.stream_issues,
        }
    }
}

/// The outcome of a finished recording.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSummaryDto {
    /// The session.
    pub session_id: String,
    /// `recorded`, or `failed` when nothing at all was captured.
    pub status: String,
    /// Recording length in milliseconds.
    pub duration_ms: u32,
    /// The stored audio tracks.
    pub tracks: Vec<TrackSummaryDto>,
    /// Frames stored.
    pub frame_count: u32,
    /// Frames dropped because processing could not keep up.
    pub dropped_frames: u32,
    /// Non-fatal problems, in order of appearance.
    pub warnings: Vec<String>,
}

impl From<RecordingSummary> for RecordingSummaryDto {
    fn from(summary: RecordingSummary) -> Self {
        let status = match summary.session.status() {
            SessionStatus::Recording => "recording",
            SessionStatus::Recorded => "recorded",
            SessionStatus::Processing => "processing",
            SessionStatus::Processed => "processed",
            SessionStatus::Failed => "failed",
        };
        Self {
            session_id: summary.session.id.to_string(),
            status: status.to_owned(),
            duration_ms: saturating_u32(summary.session.duration_ms().unwrap_or(0)),
            tracks: summary.tracks.into_iter().map(Into::into).collect(),
            frame_count: summary.frame_count,
            dropped_frames: summary.dropped_frames,
            warnings: summary.warnings,
        }
    }
}

/// A recording in progress.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRecordingDto {
    /// The session being recorded.
    pub session_id: String,
    /// Its title.
    pub title: String,
    /// Start time, milliseconds since the Unix epoch.
    pub started_at: f64,
    /// Time since it started, in milliseconds.
    pub elapsed_ms: u32,
    /// Microphone level (RMS, `0..1`).
    pub microphone_level: f32,
    /// System audio level (RMS, `0..1`).
    pub system_level: f32,
    /// Frames stored so far.
    pub frame_count: u32,
}

/// Whether a recording is running.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatusDto {
    /// `idle` or `recording`.
    pub state: String,
    /// The running recording, when `state` is `recording`.
    pub active: Option<ActiveRecordingDto>,
}

impl From<RecordingStatus> for RecordingStatusDto {
    fn from(status: RecordingStatus) -> Self {
        match status {
            RecordingStatus::Idle => Self {
                state: "idle".to_owned(),
                active: None,
            },
            RecordingStatus::Recording(active) => Self {
                state: "recording".to_owned(),
                active: Some(ActiveRecordingDto {
                    session_id: active.session_id.to_string(),
                    title: active.title,
                    // JS numbers are doubles and specta refuses 64-bit integers.
                    #[allow(clippy::cast_precision_loss)]
                    started_at: active.started_at.get() as f64,
                    elapsed_ms: saturating_u32(active.elapsed_ms),
                    microphone_level: active.microphone_level,
                    system_level: active.system_level,
                    frame_count: active.frame_count,
                }),
            },
        }
    }
}

/// Why screen capture is not available in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScreenUnavailableReasonDto {
    /// A Linux Wayland session: screen capture is off by default and only audio is recorded
    /// (the proper Wayland route is a later phase).
    WaylandUnsupported,
}

impl From<ScreenUnavailableReason> for ScreenUnavailableReasonDto {
    fn from(reason: ScreenUnavailableReason) -> Self {
        match reason {
            ScreenUnavailableReason::WaylandUnsupported => Self::WaylandUnsupported,
        }
    }
}

/// What this platform/session can record.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecordingCapabilitiesDto {
    /// Whether screen frames are captured. When `false`, `start_recording` still works and
    /// records audio only; no error is raised.
    pub screen_capture_available: bool,
    /// Why not, when `screenCaptureAvailable` is `false`.
    pub screen_unavailable_reason: Option<ScreenUnavailableReasonDto>,
}

impl From<ScreenAvailability> for RecordingCapabilitiesDto {
    fn from(availability: ScreenAvailability) -> Self {
        match availability {
            ScreenAvailability::Available => Self {
                screen_capture_available: true,
                screen_unavailable_reason: None,
            },
            ScreenAvailability::Unavailable(reason) => Self {
                screen_capture_available: false,
                screen_unavailable_reason: Some(reason.into()),
            },
        }
    }
}

/// A selectable microphone.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceDto {
    /// Pass this as `microphoneDeviceId`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether it is the system default input.
    pub is_default: bool,
}

/// A selectable display.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DisplayDto {
    /// Pass this as `displayId`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Width (macOS: points).
    pub width: u32,
    /// Height (macOS: points).
    pub height: u32,
    /// Whether it is the primary display.
    pub is_primary: bool,
}

// ---------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------

/// Emitted about four times a second while recording.
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct RecordingProgressEvent {
    /// The session being recorded.
    pub session_id: String,
    /// Time since the recording started, in milliseconds.
    pub elapsed_ms: u32,
    /// Microphone level (RMS, `0..1`).
    pub microphone_level: f32,
    /// System audio level (RMS, `0..1`).
    pub system_level: f32,
    /// Frames stored so far.
    pub frame_count: u32,
}

/// Emitted when a recording starts or ends, whoever started it (the window or the tray).
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStateEvent {
    /// `idle` or `recording`.
    pub state: String,
    /// The session that started or ended.
    pub session_id: String,
}

/// Emitted when a source has a problem but recording continues.
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct RecordingWarningEvent {
    /// The session being recorded.
    pub session_id: String,
    /// What happened (English).
    pub message: String,
}

/// Forwards the service's events to the UI and keeps the tray in step.
pub struct TauriRecordingEvents {
    app: AppHandle,
}

impl TauriRecordingEvents {
    /// Creates the forwarder for `app`.
    #[must_use]
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self { app })
    }
}

impl RecordingEvents for TauriRecordingEvents {
    fn publish(&self, event: RecordingEvent) {
        let emitted = match event {
            RecordingEvent::Started { session_id, .. } => {
                tray::set_recording(&self.app, true);
                RecordingStateEvent {
                    state: "recording".to_owned(),
                    session_id: session_id.to_string(),
                }
                .emit(&self.app)
            }
            RecordingEvent::Stopped { session_id } => {
                tray::set_recording(&self.app, false);
                RecordingStateEvent {
                    state: "idle".to_owned(),
                    session_id: session_id.to_string(),
                }
                .emit(&self.app)
            }
            RecordingEvent::Progress(progress) => RecordingProgressEvent {
                session_id: progress.session_id.to_string(),
                elapsed_ms: saturating_u32(progress.elapsed_ms),
                microphone_level: progress.microphone_level,
                system_level: progress.system_level,
                frame_count: progress.frame_count,
            }
            .emit(&self.app),
            RecordingEvent::Warning {
                session_id,
                message,
            } => {
                tracing::warn!(session = %session_id, "{message}");
                RecordingWarningEvent {
                    session_id: session_id.to_string(),
                    message,
                }
                .emit(&self.app)
            }
        };
        if let Err(error) = emitted {
            tracing::debug!(%error, "could not deliver a recording event to the UI");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------

/// Starts recording a new session.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn start_recording(
    state: State<'_, AppState>,
    options: StartRecordingOptionsDto,
) -> Result<RecordingStartedDto, RecordingIpcError> {
    let options = options.into_options(&SystemClock)?;
    let session = state.recording.start(options).await?;
    Ok(RecordingStartedDto {
        session_id: session.id.to_string(),
        title: session.title,
        #[allow(clippy::cast_precision_loss)] // see `RecordingStatusDto`
        started_at: session.started_at.get() as f64,
    })
}

/// Stops the running recording.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn stop_recording(
    state: State<'_, AppState>,
) -> Result<RecordingSummaryDto, RecordingIpcError> {
    Ok(state.recording.stop().await?.into())
}

/// Whether a recording is running, with its live numbers.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn recording_status(
    state: State<'_, AppState>,
) -> Result<RecordingStatusDto, RecordingIpcError> {
    Ok(state.recording.status().into())
}

/// What the platform can record (screen capture availability and why not).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn recording_capabilities(
    state: State<'_, AppState>,
) -> Result<RecordingCapabilitiesDto, RecordingIpcError> {
    Ok(state.recording.screen_availability().into())
}

/// Lists the selectable microphones.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_audio_devices(
    state: State<'_, AppState>,
) -> Result<Vec<AudioDeviceDto>, RecordingIpcError> {
    let audio = state.audio_capture.clone();
    let devices = blocking(move || audio.list_input_devices()).await?;
    Ok(devices
        .into_iter()
        .map(|d| AudioDeviceDto {
            id: d.id,
            name: d.name,
            is_default: d.is_default,
        })
        .collect())
}

/// Lists the capturable displays.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_displays(
    state: State<'_, AppState>,
) -> Result<Vec<DisplayDto>, RecordingIpcError> {
    let screen = state.screen_capture.clone();
    let displays = blocking(move || screen.list_displays()).await?;
    Ok(displays
        .into_iter()
        .map(|d| DisplayDto {
            id: d.id,
            name: d.name,
            width: d.width,
            height: d.height,
            is_primary: d.is_primary,
        })
        .collect())
}

/// Runs a blocking capture-backend call off the async executor.
async fn blocking<T, F>(work: F) -> Result<T, RecordingIpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, tan_application::ports::CaptureError> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result.map_err(|e| RecordingError::Capture(e).into()),
        Err(join_error) => {
            log_source("capture worker failed", &join_error);
            Err(RecordingIpcError {
                code: RecordingErrorCode::Internal,
                message: "the capture worker failed".to_owned(),
            })
        }
    }
}

// ---------------------------------------------------------------------------------------
// Tray entry points
// ---------------------------------------------------------------------------------------

/// Starts a recording with default options (tray menu).
pub async fn start_with_defaults(app: &AppHandle) -> Result<(), RecordingIpcError> {
    let state = app.state::<AppState>();
    let options = StartRecordingOptionsDto::default().into_options(&SystemClock)?;
    state.recording.start(options).await?;
    Ok(())
}

/// Stops the running recording (tray menu, quit).
pub async fn stop_running(app: &AppHandle) -> Result<(), RecordingIpcError> {
    let state = app.state::<AppState>();
    state.recording.stop().await?;
    Ok(())
}

/// How long [`stop_and_settle`] waits for a recording that is still starting or stopping.
const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Stops the running recording and waits until the service is idle again, so the process can
/// exit without losing audio or leaving the session in `recording`.
///
/// A recording that is still *starting* is waited for and stopped the moment it is running
/// (the state goes `Starting -> Recording`, never straight to idle); one that is already
/// stopping is waited for. Gives up after [`SETTLE_TIMEOUT`].
pub async fn stop_and_settle(app: &AppHandle) {
    let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
    while is_busy(app) && std::time::Instant::now() < deadline {
        if is_recording(app) {
            if let Err(error) = stop_running(app).await {
                // Losing a race with another stop is fine; anything else is worth a log.
                if error.code != RecordingErrorCode::NotRecording {
                    tracing::error!(code = ?error.code, "{}", error.message);
                    break;
                }
            }
        } else {
            // Starting or stopping: look again shortly (off the async executor).
            let _ = tauri::async_runtime::spawn_blocking(|| {
                std::thread::sleep(std::time::Duration::from_millis(50));
            })
            .await;
        }
    }
    if is_busy(app) {
        tracing::warn!("the recording did not settle in time; it will be recovered at next start");
    }
}

/// Whether a recording is running (or starting/stopping).
#[must_use]
pub fn is_busy(app: &AppHandle) -> bool {
    app.state::<AppState>().recording.is_busy()
}

/// Whether a recording is running right now.
#[must_use]
pub fn is_recording(app: &AppHandle) -> bool {
    matches!(
        app.state::<AppState>().recording.status(),
        RecordingStatus::Recording(_)
    )
}
