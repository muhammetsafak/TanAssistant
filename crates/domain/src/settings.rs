//! Typed application settings. All defaults live here, in one place.

use std::error::Error;
use std::fmt;

use crate::language::{LanguageCode, LanguageSetting, UiLanguage};

/// Smallest allowed frame capture interval.
pub const MIN_FRAME_INTERVAL_MS: u32 = 500;
/// Largest allowed frame capture interval.
pub const MAX_FRAME_INTERVAL_MS: u32 = 60_000;
/// Largest meaningful perceptual-hash distance (the hash has 64 bits).
pub const MAX_FRAME_CHANGE_THRESHOLD: u32 = 64;

/// User-configurable application settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    /// Language of the application UI.
    pub ui_language: UiLanguage,
    /// Language notes are generated in unless a session overrides it.
    pub default_note_language: LanguageCode,
    /// Spoken-language setting for new sessions.
    pub default_speech_language: LanguageSetting,
    /// How often the screen is sampled, in milliseconds.
    pub frame_interval_ms: u32,
    /// Minimum perceptual-hash (Hamming) distance for a frame to count as changed.
    pub frame_change_threshold: u32,
    /// Start the pipeline automatically when a recording ends.
    pub auto_process_after_recording: bool,
    /// Delete audio files this many days after processing; `None` keeps them forever.
    pub audio_retention_days: Option<u32>,
    /// Whether the user acknowledged the recording-consent notice (shown once).
    pub consent_notice_acknowledged: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            ui_language: UiLanguage::Tr,
            default_note_language: LanguageCode::turkish(),
            default_speech_language: LanguageSetting::Auto,
            frame_interval_ms: 3_000,
            frame_change_threshold: 8,
            auto_process_after_recording: true,
            audio_retention_days: None,
            consent_notice_acknowledged: false,
        }
    }
}

/// A partial update: only the `Some` fields change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsPatch {
    /// New UI language.
    pub ui_language: Option<UiLanguage>,
    /// New default note language.
    pub default_note_language: Option<LanguageCode>,
    /// New default spoken-language setting.
    pub default_speech_language: Option<LanguageSetting>,
    /// New frame interval.
    pub frame_interval_ms: Option<u32>,
    /// New frame change threshold.
    pub frame_change_threshold: Option<u32>,
    /// New auto-process flag.
    pub auto_process_after_recording: Option<bool>,
    /// New retention: `Some(None)` switches retention off, `Some(Some(n))` sets `n` days.
    pub audio_retention_days: Option<Option<u32>>,
    /// New consent-acknowledged flag.
    pub consent_notice_acknowledged: Option<bool>,
}

/// A settings value is out of its allowed range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    /// `frame_interval_ms` outside [`MIN_FRAME_INTERVAL_MS`], [`MAX_FRAME_INTERVAL_MS`].
    FrameIntervalOutOfRange(u32),
    /// `frame_change_threshold` above [`MAX_FRAME_CHANGE_THRESHOLD`].
    FrameThresholdOutOfRange(u32),
    /// `audio_retention_days` set to zero.
    RetentionMustBePositive,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameIntervalOutOfRange(v) => write!(
                f,
                "frame_interval_ms {v} is outside {MIN_FRAME_INTERVAL_MS}..={MAX_FRAME_INTERVAL_MS}"
            ),
            Self::FrameThresholdOutOfRange(v) => {
                write!(
                    f,
                    "frame_change_threshold {v} is above {MAX_FRAME_CHANGE_THRESHOLD}"
                )
            }
            Self::RetentionMustBePositive => f.write_str("audio_retention_days must be at least 1"),
        }
    }
}

impl Error for SettingsError {}

impl AppSettings {
    /// Checks every value against its allowed range.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(MIN_FRAME_INTERVAL_MS..=MAX_FRAME_INTERVAL_MS).contains(&self.frame_interval_ms) {
            return Err(SettingsError::FrameIntervalOutOfRange(
                self.frame_interval_ms,
            ));
        }
        if self.frame_change_threshold > MAX_FRAME_CHANGE_THRESHOLD {
            return Err(SettingsError::FrameThresholdOutOfRange(
                self.frame_change_threshold,
            ));
        }
        if self.audio_retention_days == Some(0) {
            return Err(SettingsError::RetentionMustBePositive);
        }
        Ok(())
    }

    /// Applies `patch` on top of `self` and validates the result. `self` is left untouched on
    /// error.
    pub fn patched(&self, patch: &SettingsPatch) -> Result<Self, SettingsError> {
        let mut next = self.clone();
        if let Some(v) = patch.ui_language {
            next.ui_language = v;
        }
        if let Some(v) = &patch.default_note_language {
            next.default_note_language = v.clone();
        }
        if let Some(v) = &patch.default_speech_language {
            next.default_speech_language = v.clone();
        }
        if let Some(v) = patch.frame_interval_ms {
            next.frame_interval_ms = v;
        }
        if let Some(v) = patch.frame_change_threshold {
            next.frame_change_threshold = v;
        }
        if let Some(v) = patch.auto_process_after_recording {
            next.auto_process_after_recording = v;
        }
        if let Some(v) = patch.audio_retention_days {
            next.audio_retention_days = v;
        }
        if let Some(v) = patch.consent_notice_acknowledged {
            next.consent_notice_acknowledged = v;
        }
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, SettingsError, SettingsPatch};
    use crate::language::{LanguageCode, LanguageSetting, UiLanguage};

    #[test]
    fn defaults_are_valid_and_turkish_first() {
        let d = AppSettings::default();
        d.validate().unwrap();
        assert_eq!(d.ui_language, UiLanguage::Tr);
        assert_eq!(d.default_note_language, LanguageCode::turkish());
        assert_eq!(d.default_speech_language, LanguageSetting::Auto);
        assert_eq!(d.audio_retention_days, None);
        assert!(!d.consent_notice_acknowledged);
    }

    #[test]
    fn patch_changes_only_given_fields() {
        let base = AppSettings::default();
        let next = base
            .patched(&SettingsPatch {
                ui_language: Some(UiLanguage::En),
                audio_retention_days: Some(Some(30)),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert_eq!(next.ui_language, UiLanguage::En);
        assert_eq!(next.audio_retention_days, Some(30));
        assert_eq!(next.frame_interval_ms, base.frame_interval_ms);
        assert_eq!(next.default_note_language, base.default_note_language);

        let cleared = next
            .patched(&SettingsPatch {
                audio_retention_days: Some(None),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert_eq!(cleared.audio_retention_days, None);
    }

    #[test]
    fn invalid_patch_is_rejected() {
        let base = AppSettings::default();
        let bad_interval = SettingsPatch {
            frame_interval_ms: Some(10),
            ..SettingsPatch::default()
        };
        assert_eq!(
            base.patched(&bad_interval),
            Err(SettingsError::FrameIntervalOutOfRange(10))
        );
        let bad_threshold = SettingsPatch {
            frame_change_threshold: Some(65),
            ..SettingsPatch::default()
        };
        assert_eq!(
            base.patched(&bad_threshold),
            Err(SettingsError::FrameThresholdOutOfRange(65))
        );
        let zero_days = SettingsPatch {
            audio_retention_days: Some(Some(0)),
            ..SettingsPatch::default()
        };
        assert_eq!(
            base.patched(&zero_days),
            Err(SettingsError::RetentionMustBePositive)
        );
    }
}
