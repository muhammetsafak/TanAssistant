//! IPC commands for application settings.

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_domain::{AppSettings, LanguageCode, LanguageSetting, SettingsPatch, UiLanguage};
use tauri::State;

use crate::error::IpcError;
use crate::state::AppState;

/// Language of the application UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum UiLanguageDto {
    /// Turkish.
    Tr,
    /// English.
    En,
}

impl From<UiLanguage> for UiLanguageDto {
    fn from(language: UiLanguage) -> Self {
        match language {
            UiLanguage::Tr => Self::Tr,
            UiLanguage::En => Self::En,
        }
    }
}

impl From<UiLanguageDto> for UiLanguage {
    fn from(language: UiLanguageDto) -> Self {
        match language {
            UiLanguageDto::Tr => Self::Tr,
            UiLanguageDto::En => Self::En,
        }
    }
}

/// The application settings.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsDto {
    /// UI language.
    pub ui_language: UiLanguageDto,
    /// Language notes are generated in (a language code such as `tr`).
    pub default_note_language: String,
    /// Spoken language of new sessions: `auto` or a language code.
    pub default_speech_language: String,
    /// How often the screen is sampled, in milliseconds.
    pub frame_interval_ms: u32,
    /// Minimum perceptual-hash distance for a frame to count as changed.
    pub frame_change_threshold: u32,
    /// Start processing automatically when a recording ends.
    pub auto_process_after_recording: bool,
    /// Delete audio files this many days after processing; null keeps them forever.
    pub audio_retention_days: Option<u32>,
    /// Whether the recording-consent notice was acknowledged.
    pub consent_notice_acknowledged: bool,
    /// Check for a new release at startup and now and then while the app runs.
    pub auto_check_updates: bool,
}

impl From<AppSettings> for AppSettingsDto {
    fn from(settings: AppSettings) -> Self {
        Self {
            ui_language: settings.ui_language.into(),
            default_note_language: settings.default_note_language.to_string(),
            default_speech_language: settings.default_speech_language.to_storage_string(),
            frame_interval_ms: settings.frame_interval_ms,
            frame_change_threshold: settings.frame_change_threshold,
            auto_process_after_recording: settings.auto_process_after_recording,
            audio_retention_days: settings.audio_retention_days,
            consent_notice_acknowledged: settings.consent_notice_acknowledged,
            auto_check_updates: settings.auto_check_updates,
        }
    }
}

/// New audio retention. `days: null` switches retention off (keep audio forever).
#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPatchDto {
    /// Days to keep audio after processing, or null to keep it forever.
    pub days: Option<u32>,
}

/// A partial settings update: only the fields that are present change.
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatchDto {
    /// New UI language.
    pub ui_language: Option<UiLanguageDto>,
    /// New note language code.
    pub default_note_language: Option<String>,
    /// New spoken-language setting: `auto` or a language code.
    pub default_speech_language: Option<String>,
    /// New frame interval in milliseconds.
    pub frame_interval_ms: Option<u32>,
    /// New frame change threshold.
    pub frame_change_threshold: Option<u32>,
    /// New auto-process flag.
    pub auto_process_after_recording: Option<bool>,
    /// New audio retention.
    pub audio_retention: Option<RetentionPatchDto>,
    /// New consent-acknowledged flag.
    pub consent_notice_acknowledged: Option<bool>,
    /// New automatic-update-check flag.
    pub auto_check_updates: Option<bool>,
}

impl SettingsPatchDto {
    fn into_patch(self) -> Result<SettingsPatch, IpcError> {
        let note_language = self
            .default_note_language
            .as_deref()
            .map(LanguageCode::new)
            .transpose()
            .map_err(|e| IpcError::validation(e.to_string()))?;
        let speech_language = self
            .default_speech_language
            .as_deref()
            .map(str::parse::<LanguageSetting>)
            .transpose()
            .map_err(|e| IpcError::validation(e.to_string()))?;
        Ok(SettingsPatch {
            ui_language: self.ui_language.map(UiLanguage::from),
            default_note_language: note_language,
            default_speech_language: speech_language,
            frame_interval_ms: self.frame_interval_ms,
            frame_change_threshold: self.frame_change_threshold,
            auto_process_after_recording: self.auto_process_after_recording,
            audio_retention_days: self.audio_retention.map(|r| r.days),
            consent_notice_acknowledged: self.consent_notice_acknowledged,
            auto_check_updates: self.auto_check_updates,
        })
    }
}

/// Returns the current settings.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettingsDto, IpcError> {
    Ok(state.get_settings.execute().await?.into())
}

/// Applies a partial settings update and returns the resulting settings.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn update_settings(
    state: State<'_, AppState>,
    patch: SettingsPatchDto,
) -> Result<AppSettingsDto, IpcError> {
    let patch = patch.into_patch()?;
    Ok(state.update_settings.execute(&patch).await?.into())
}
