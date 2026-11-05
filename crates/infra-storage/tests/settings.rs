// `assert!(x.is_empty())` / `assert!(a == b)` read fine in tests; pedantic clippy would rewrite them.
#![allow(clippy::assert_is_empty, clippy::manual_assert_eq)]

mod common;

use common::{block_on, test_db};
use tan_application::ports::SettingsRepository;
use tan_application::RepositoryError;
use tan_domain::{AppSettings, LanguageCode, LanguageSetting, SettingsPatch, UiLanguage};
use tan_infra_storage::{Database, SqliteSettingsRepository};

#[test]
fn empty_database_returns_domain_defaults() {
    let t = test_db();
    let repo = SqliteSettingsRepository::new(t.db.clone());
    assert_eq!(block_on(repo.load()).unwrap(), AppSettings::default());
}

#[test]
fn partial_update_keeps_other_fields_and_persists() {
    let t = test_db();
    let repo = SqliteSettingsRepository::new(t.db.clone());

    let after_first = block_on(repo.update(&SettingsPatch {
        ui_language: Some(UiLanguage::En),
        frame_interval_ms: Some(5_000),
        ..SettingsPatch::default()
    }))
    .unwrap();
    assert_eq!(after_first.ui_language, UiLanguage::En);
    assert_eq!(after_first.frame_interval_ms, 5_000);

    let after_second = block_on(repo.update(&SettingsPatch {
        audio_retention_days: Some(Some(14)),
        default_speech_language: Some(LanguageSetting::Fixed(LanguageCode::english())),
        consent_notice_acknowledged: Some(true),
        ..SettingsPatch::default()
    }))
    .unwrap();

    // Fields from the first update survive the second one.
    assert_eq!(after_second.ui_language, UiLanguage::En);
    assert_eq!(after_second.frame_interval_ms, 5_000);
    assert_eq!(after_second.audio_retention_days, Some(14));
    assert!(after_second.consent_notice_acknowledged);
    // Untouched fields still equal the defaults.
    let defaults = AppSettings::default();
    assert_eq!(
        after_second.default_note_language,
        defaults.default_note_language
    );
    assert_eq!(
        after_second.frame_change_threshold,
        defaults.frame_change_threshold
    );
    assert_eq!(
        after_second.auto_process_after_recording,
        defaults.auto_process_after_recording
    );

    // A fresh repository over a reopened file sees the same values.
    let reopened = SqliteSettingsRepository::new(Database::open(&t.path).unwrap());
    assert_eq!(block_on(reopened.load()).unwrap(), after_second);
}

#[test]
fn retention_can_be_switched_off_again() {
    let t = test_db();
    let repo = SqliteSettingsRepository::new(t.db.clone());
    block_on(repo.update(&SettingsPatch {
        audio_retention_days: Some(Some(7)),
        ..SettingsPatch::default()
    }))
    .unwrap();
    let cleared = block_on(repo.update(&SettingsPatch {
        audio_retention_days: Some(None),
        ..SettingsPatch::default()
    }))
    .unwrap();
    assert_eq!(cleared.audio_retention_days, None);
    assert_eq!(block_on(repo.load()).unwrap().audio_retention_days, None);
}

#[test]
fn invalid_patch_is_rejected_and_changes_nothing() {
    let t = test_db();
    let repo = SqliteSettingsRepository::new(t.db.clone());
    block_on(repo.update(&SettingsPatch {
        ui_language: Some(UiLanguage::En),
        ..SettingsPatch::default()
    }))
    .unwrap();

    let err = block_on(repo.update(&SettingsPatch {
        frame_interval_ms: Some(1),
        default_note_language: Some(LanguageCode::english()),
        ..SettingsPatch::default()
    }))
    .unwrap_err();
    assert!(matches!(err, RepositoryError::Invalid(_)), "{err}");

    let loaded = block_on(repo.load()).unwrap();
    assert_eq!(loaded.ui_language, UiLanguage::En);
    assert_eq!(
        loaded.frame_interval_ms,
        AppSettings::default().frame_interval_ms
    );
    assert_eq!(
        loaded.default_note_language,
        AppSettings::default().default_note_language
    );
}

#[test]
fn corrupt_stored_value_is_reported_not_silently_defaulted() {
    let t = test_db();
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('frame_interval_ms', 'soon')",
        [],
    )
    .unwrap();
    // Unknown keys from a newer app version are ignored.
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('future_option', '1')",
        [],
    )
    .unwrap();
    drop(conn);

    let repo = SqliteSettingsRepository::new(t.db.clone());
    assert!(matches!(
        block_on(repo.load()),
        Err(RepositoryError::CorruptData(_))
    ));
}

#[test]
fn out_of_range_stored_values_are_reported_and_a_patch_can_repair_them() {
    let t = test_db();
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    for (key, value) in [("frame_interval_ms", "0"), ("audio_retention_days", "0")] {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .unwrap();
    }
    drop(conn);

    let repo = SqliteSettingsRepository::new(t.db.clone());
    assert!(matches!(
        block_on(repo.load()),
        Err(RepositoryError::CorruptData(_))
    ));

    // A patch that leaves a bad value in place is rejected...
    let err = block_on(repo.update(&SettingsPatch {
        ui_language: Some(UiLanguage::En),
        ..SettingsPatch::default()
    }))
    .unwrap_err();
    assert!(matches!(err, RepositoryError::Invalid(_)), "{err}");

    // ...while one that fixes every bad value goes through and heals the store.
    block_on(repo.update(&SettingsPatch {
        frame_interval_ms: Some(2_000),
        audio_retention_days: Some(None),
        ..SettingsPatch::default()
    }))
    .unwrap();
    let healed = block_on(repo.load()).unwrap();
    assert_eq!(healed.frame_interval_ms, 2_000);
    assert_eq!(healed.audio_retention_days, None);
}
