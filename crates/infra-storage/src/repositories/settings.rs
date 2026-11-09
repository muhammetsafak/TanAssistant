//! `SettingsRepository` over SQLite.
//!
//! Settings are stored sparsely as key/value rows. A key without a row means "use the domain
//! default", so defaults exist in exactly one place (`AppSettings::default`) and a partial
//! update writes only the keys it mentions.

use rusqlite::{params, Connection};
use tan_application::ports::SettingsRepository;
use tan_application::RepositoryError;
use tan_domain::{AppSettings, SettingsPatch};

use crate::db::{Database, DbError};

const UI_LANGUAGE: &str = "ui_language";
const DEFAULT_NOTE_LANGUAGE: &str = "default_note_language";
const DEFAULT_SPEECH_LANGUAGE: &str = "default_speech_language";
const FRAME_INTERVAL_MS: &str = "frame_interval_ms";
const FRAME_CHANGE_THRESHOLD: &str = "frame_change_threshold";
const AUTO_PROCESS_AFTER_RECORDING: &str = "auto_process_after_recording";
const AUDIO_RETENTION_DAYS: &str = "audio_retention_days";
const CONSENT_NOTICE_ACKNOWLEDGED: &str = "consent_notice_acknowledged";
const AUTO_CHECK_UPDATES: &str = "auto_check_updates";

fn corrupt(key: &str, value: &str, reason: impl std::fmt::Display) -> DbError {
    RepositoryError::CorruptData(format!(
        "setting {key} has invalid value {value:?}: {reason}"
    ))
    .into()
}

fn parse_bool(key: &str, value: &str) -> Result<bool, DbError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(corrupt(key, other, "expected true or false")),
    }
}

fn parse_u32(key: &str, value: &str) -> Result<u32, DbError> {
    value.parse().map_err(|e| corrupt(key, value, e))
}

/// Overlays one stored row on `settings`. Unknown keys (from a newer version) are ignored.
fn apply_stored(settings: &mut AppSettings, key: &str, value: &str) -> Result<(), DbError> {
    match key {
        UI_LANGUAGE => settings.ui_language = value.parse().map_err(|e| corrupt(key, value, e))?,
        DEFAULT_NOTE_LANGUAGE => {
            settings.default_note_language = value.parse().map_err(|e| corrupt(key, value, e))?;
        }
        DEFAULT_SPEECH_LANGUAGE => {
            settings.default_speech_language = value.parse().map_err(|e| corrupt(key, value, e))?;
        }
        FRAME_INTERVAL_MS => settings.frame_interval_ms = parse_u32(key, value)?,
        FRAME_CHANGE_THRESHOLD => settings.frame_change_threshold = parse_u32(key, value)?,
        AUTO_PROCESS_AFTER_RECORDING => {
            settings.auto_process_after_recording = parse_bool(key, value)?;
        }
        AUDIO_RETENTION_DAYS => settings.audio_retention_days = Some(parse_u32(key, value)?),
        CONSENT_NOTICE_ACKNOWLEDGED => {
            settings.consent_notice_acknowledged = parse_bool(key, value)?;
        }
        AUTO_CHECK_UPDATES => settings.auto_check_updates = parse_bool(key, value)?,
        _ => {}
    }
    Ok(())
}

/// Reads the stored rows over the defaults without range-checking the result.
fn load_unvalidated(conn: &Connection) -> Result<AppSettings, DbError> {
    let mut settings = AppSettings::default();
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, value) in rows {
        apply_stored(&mut settings, &key, &value)?;
    }
    Ok(settings)
}

/// Reads the settings and rejects stored values outside the domain's allowed ranges, the same
/// way an unparsable value is rejected.
fn load_settings(conn: &Connection) -> Result<AppSettings, DbError> {
    let settings = load_unvalidated(conn)?;
    settings.validate().map_err(|e| {
        RepositoryError::CorruptData(format!("stored settings are out of range: {e}"))
    })?;
    Ok(settings)
}

fn put(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
}

fn remove(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
        .map(|_| ())
}

/// Writes the rows for every field present in `patch`, taking values from the validated
/// `next` settings.
fn write_patch(
    conn: &Connection,
    patch: &SettingsPatch,
    next: &AppSettings,
) -> rusqlite::Result<()> {
    if patch.ui_language.is_some() {
        put(conn, UI_LANGUAGE, next.ui_language.as_str())?;
    }
    if patch.default_note_language.is_some() {
        put(
            conn,
            DEFAULT_NOTE_LANGUAGE,
            next.default_note_language.as_str(),
        )?;
    }
    if patch.default_speech_language.is_some() {
        put(
            conn,
            DEFAULT_SPEECH_LANGUAGE,
            &next.default_speech_language.to_storage_string(),
        )?;
    }
    if patch.frame_interval_ms.is_some() {
        put(conn, FRAME_INTERVAL_MS, &next.frame_interval_ms.to_string())?;
    }
    if patch.frame_change_threshold.is_some() {
        put(
            conn,
            FRAME_CHANGE_THRESHOLD,
            &next.frame_change_threshold.to_string(),
        )?;
    }
    if patch.auto_process_after_recording.is_some() {
        put(
            conn,
            AUTO_PROCESS_AFTER_RECORDING,
            &next.auto_process_after_recording.to_string(),
        )?;
    }
    if patch.audio_retention_days.is_some() {
        match next.audio_retention_days {
            Some(days) => put(conn, AUDIO_RETENTION_DAYS, &days.to_string())?,
            None => remove(conn, AUDIO_RETENTION_DAYS)?,
        }
    }
    if patch.consent_notice_acknowledged.is_some() {
        put(
            conn,
            CONSENT_NOTICE_ACKNOWLEDGED,
            &next.consent_notice_acknowledged.to_string(),
        )?;
    }
    if patch.auto_check_updates.is_some() {
        put(
            conn,
            AUTO_CHECK_UPDATES,
            &next.auto_check_updates.to_string(),
        )?;
    }
    Ok(())
}

/// SQLite-backed [`SettingsRepository`].
#[derive(Debug, Clone)]
pub struct SqliteSettingsRepository {
    db: Database,
}

impl SqliteSettingsRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl SettingsRepository for SqliteSettingsRepository {
    async fn load(&self) -> Result<AppSettings, RepositoryError> {
        self.db.with_conn(|conn| load_settings(conn))
    }

    async fn update(&self, patch: &SettingsPatch) -> Result<AppSettings, RepositoryError> {
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            // Unvalidated on purpose: a patch must be able to repair an out-of-range stored value;
            // `patched` validates the outcome.
            let current = load_unvalidated(&tx)?;
            let next = current
                .patched(patch)
                .map_err(|e| RepositoryError::Invalid(e.to_string()))?;
            write_patch(&tx, patch, &next)?;
            tx.commit()?;
            Ok(next)
        })
    }
}
