//! Behaviour of the master-password-encrypted file store.

use futures_executor::block_on;
use tan_application::ports::{SecretStore, SecretStoreControl, SecretStoreError, SecretValue};
use tan_domain::SecretId;
use tan_infra_secrets::{EncryptedFileSecretStore, KdfParams};

const PASSWORD: &str = "correct horse battery staple";
const API_KEY: &str = "sk-test-PLAINTEXT-MARKER-0123456789";

fn store_at(dir: &tempfile::TempDir) -> EncryptedFileSecretStore {
    EncryptedFileSecretStore::with_params(
        dir.path().join("secrets.bin"),
        KdfParams::INSECURE_FAST_FOR_TESTS,
    )
}

fn pw(text: &str) -> SecretValue {
    SecretValue::new(text)
}

fn id(text: &str) -> SecretId {
    SecretId::new(text)
}

/// A store with a master password already set (so it is unlocked) and one secret in it.
fn populated(dir: &tempfile::TempDir) -> EncryptedFileSecretStore {
    let store = store_at(dir);
    store.set_master_password(&pw(PASSWORD), None).unwrap();
    block_on(store.put(&id("one"), &SecretValue::new(API_KEY))).unwrap();
    store
}

#[test]
fn operations_need_a_master_password_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let status = store.status();
    assert!(status.locked);
    assert!(!status.master_password_set);
    assert!(matches!(
        block_on(store.get(&id("one"))),
        Err(SecretStoreError::NotConfigured)
    ));
    assert!(matches!(
        store.unlock(&pw(PASSWORD)),
        Err(SecretStoreError::NotConfigured)
    ));
}

#[test]
fn put_get_delete_round_trip_across_reopening() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));

    let reopened = store_at(&dir);
    assert!(reopened.status().locked);
    assert!(reopened.status().master_password_set);
    assert!(matches!(
        block_on(reopened.get(&id("one"))),
        Err(SecretStoreError::Locked)
    ));

    reopened.unlock(&pw(PASSWORD)).unwrap();
    let value = block_on(reopened.get(&id("one"))).unwrap().unwrap();
    assert_eq!(value.expose(), API_KEY);
    assert!(block_on(reopened.get(&id("missing"))).unwrap().is_none());

    block_on(reopened.delete(&id("one"))).unwrap();
    block_on(reopened.delete(&id("one"))).unwrap(); // idempotent
    assert!(block_on(reopened.get(&id("one"))).unwrap().is_none());

    let again = store_at(&dir);
    again.unlock(&pw(PASSWORD)).unwrap();
    assert!(block_on(again.get(&id("one"))).unwrap().is_none());
}

#[test]
fn put_replaces_an_existing_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = populated(&dir);
    block_on(store.put(&id("one"), &pw("second"))).unwrap();
    assert_eq!(
        block_on(store.get(&id("one"))).unwrap().unwrap().expose(),
        "second"
    );
}

#[test]
fn wrong_password_does_not_unlock() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));

    let store = store_at(&dir);
    assert!(matches!(
        store.unlock(&pw("not the password")),
        Err(SecretStoreError::InvalidPassword)
    ));
    assert!(store.status().locked);
    assert!(matches!(
        block_on(store.get(&id("one"))),
        Err(SecretStoreError::Locked)
    ));
}

#[test]
fn file_bytes_contain_no_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));
    let bytes = std::fs::read(dir.path().join("secrets.bin")).unwrap();

    let contains = |needle: &str| {
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    };
    assert!(!contains(API_KEY), "secret value found in the file");
    assert!(!contains("PLAINTEXT-MARKER"), "part of the value found");
    assert!(!contains(PASSWORD), "master password found in the file");
}

#[test]
fn every_write_uses_a_fresh_nonce() {
    let dir = tempfile::tempdir().unwrap();
    let store = populated(&dir);
    let first = std::fs::read(dir.path().join("secrets.bin")).unwrap();
    // Same id, same value, written again: the ciphertext must differ.
    block_on(store.put(&id("one"), &SecretValue::new(API_KEY))).unwrap();
    let second = std::fs::read(dir.path().join("secrets.bin")).unwrap();
    assert_eq!(first.len(), second.len());
    assert_ne!(first, second);
}

#[test]
fn a_tampered_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));
    let path = dir.path().join("secrets.bin");
    let original = std::fs::read(&path).unwrap();

    // Flip one bit at several positions: header, verifier, record body, footer. Each must be
    // refused (a wrong-password style error or a corruption error, never a successful unlock).
    for position in [10, 30, 60, original.len() / 2, original.len() - 5] {
        let mut damaged = original.clone();
        damaged[position] ^= 0x01;
        std::fs::write(&path, &damaged).unwrap();
        let store = store_at(&dir);
        let result = store.unlock(&pw(PASSWORD));
        assert!(
            matches!(
                result,
                Err(SecretStoreError::Corrupt(_) | SecretStoreError::InvalidPassword)
            ),
            "bit flip at byte {position} was not detected: {result:?}"
        );
        assert!(store.status().locked);
    }

    // A flipped bit inside the record region is specifically an integrity failure.
    let mut damaged = original.clone();
    let record_byte = original.len() - 24 - 16 - 10;
    damaged[record_byte] ^= 0x80;
    std::fs::write(&path, &damaged).unwrap();
    assert!(matches!(
        store_at(&dir).unlock(&pw(PASSWORD)),
        Err(SecretStoreError::Corrupt(_))
    ));
}

#[test]
fn truncated_extended_and_foreign_files_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));
    let path = dir.path().join("secrets.bin");
    let original = std::fs::read(&path).unwrap();

    let truncated = &original[..original.len() - 1];
    std::fs::write(&path, truncated).unwrap();
    assert!(matches!(
        store_at(&dir).unlock(&pw(PASSWORD)),
        Err(SecretStoreError::Corrupt(_))
    ));

    let mut extended = original.clone();
    extended.push(0);
    std::fs::write(&path, &extended).unwrap();
    assert!(matches!(
        store_at(&dir).unlock(&pw(PASSWORD)),
        Err(SecretStoreError::Corrupt(_))
    ));

    std::fs::write(&path, b"this is not a secret store").unwrap();
    assert!(matches!(
        store_at(&dir).unlock(&pw(PASSWORD)),
        Err(SecretStoreError::Corrupt(_))
    ));

    std::fs::write(&path, b"").unwrap();
    assert!(matches!(
        store_at(&dir).unlock(&pw(PASSWORD)),
        Err(SecretStoreError::Corrupt(_))
    ));
}

#[test]
fn changing_the_master_password_keeps_the_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let store = populated(&dir);

    // The current password is required and must be right.
    assert!(matches!(
        store.set_master_password(&pw("a brand new password"), None),
        Err(SecretStoreError::InvalidPassword)
    ));
    assert!(matches!(
        store.set_master_password(&pw("a brand new password"), Some(&pw("wrong"))),
        Err(SecretStoreError::InvalidPassword)
    ));

    store
        .set_master_password(&pw("a brand new password"), Some(&pw(PASSWORD)))
        .unwrap();
    assert_eq!(
        block_on(store.get(&id("one"))).unwrap().unwrap().expose(),
        API_KEY
    );

    let reopened = store_at(&dir);
    assert!(matches!(
        reopened.unlock(&pw(PASSWORD)),
        Err(SecretStoreError::InvalidPassword)
    ));
    reopened.unlock(&pw("a brand new password")).unwrap();
    assert_eq!(
        block_on(reopened.get(&id("one")))
            .unwrap()
            .unwrap()
            .expose(),
        API_KEY
    );
}

#[test]
fn locking_makes_values_unreachable_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = populated(&dir);
    store.lock();
    assert!(store.status().locked);
    assert!(matches!(
        block_on(store.get(&id("one"))),
        Err(SecretStoreError::Locked)
    ));
    store.unlock(&pw(PASSWORD)).unwrap();
    assert!(block_on(store.get(&id("one"))).unwrap().is_some());
}

#[test]
fn errors_never_contain_values() {
    let dir = tempfile::tempdir().unwrap();
    drop(populated(&dir));
    let path = dir.path().join("secrets.bin");
    let mut damaged = std::fs::read(&path).unwrap();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    std::fs::write(&path, &damaged).unwrap();

    let error = store_at(&dir).unlock(&pw(PASSWORD)).unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(!text.contains(API_KEY));
    assert!(!text.contains(PASSWORD));
}
