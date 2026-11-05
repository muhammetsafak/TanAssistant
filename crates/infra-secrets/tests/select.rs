//! Backend selection and the remembered choice.

use tan_application::ports::{SecretBackend, SecretStoreControl, SecretStoreError};
use tan_infra_secrets::{
    open_secret_store_with, KdfParams, KeyringSecretStore, SelectedSecretStore,
    ENCRYPTED_FILE_NAME, PREFERENCE_FILE_NAME, SERVICE_NAME,
};

fn test_vault() -> KeyringSecretStore {
    KeyringSecretStore::with_service(format!("{SERVICE_NAME}.test-select-{}", std::process::id()))
}

#[test]
fn a_remembered_file_backend_is_used_without_probing_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PREFERENCE_FILE_NAME), "encrypted_file\n").unwrap();

    let store =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS)
            .unwrap();
    assert!(matches!(store, SelectedSecretStore::EncryptedFile(_)));
    let status = store.status();
    assert_eq!(status.backend, SecretBackend::EncryptedFile);
    assert!(status.locked);
    assert!(!status.master_password_set);
}

#[test]
fn the_first_choice_is_persisted_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let first =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS)
            .unwrap();
    let backend = first.status().backend;
    let saved = std::fs::read_to_string(dir.path().join(PREFERENCE_FILE_NAME)).unwrap();
    assert_eq!(saved.trim(), backend.as_str());

    let second =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS)
            .unwrap();
    assert_eq!(second.status().backend, backend);
}

#[test]
fn an_unknown_preference_is_an_error_not_a_silent_switch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PREFERENCE_FILE_NAME), "floppy-disk").unwrap();
    let result =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS);
    assert!(matches!(result, Err(SecretStoreError::Corrupt(_))));
}

#[test]
fn the_file_backend_creates_secrets_bin_next_to_the_preference() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PREFERENCE_FILE_NAME), "encrypted_file").unwrap();
    let store =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS)
            .unwrap();
    store
        .set_master_password(
            &tan_application::ports::SecretValue::new("long enough pw"),
            None,
        )
        .unwrap();
    assert!(dir.path().join(ENCRYPTED_FILE_NAME).is_file());
}

#[test]
fn an_empty_preference_file_means_nothing_was_remembered() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PREFERENCE_FILE_NAME), "").unwrap();
    let store =
        open_secret_store_with(dir.path(), test_vault(), KdfParams::INSECURE_FAST_FOR_TESTS)
            .unwrap();
    let saved = std::fs::read_to_string(dir.path().join(PREFERENCE_FILE_NAME)).unwrap();
    assert_eq!(saved.trim(), store.status().backend.as_str());
    assert!(!dir
        .path()
        .join(format!("{PREFERENCE_FILE_NAME}.tmp"))
        .exists());
}
