//! Chooses the secret store backend and remembers the choice.
//!
//! The first run picks the OS vault when it works, otherwise the encrypted file. The choice is
//! then **persisted** and never silently re-evaluated: switching backend behind the user's
//! back would make every stored secret appear to vanish.
//!
//! Persistence is a one-line file (`secret-backend`) in the data directory next to the
//! database: `tan-infra-secrets` may not depend on the storage adapter, and the settings table
//! is typed to `AppSettings`.

use std::fs;
use std::io::Write;
use std::path::Path;

use tan_application::ports::{
    SecretBackend, SecretStore, SecretStoreControl, SecretStoreError, SecretStoreResult,
    SecretStoreStatus, SecretValue,
};
use tan_domain::SecretId;

use crate::encrypted_file::{EncryptedFileSecretStore, KdfParams};
use crate::keyring_store::KeyringSecretStore;

/// File name of the encrypted fallback store inside the data directory.
pub const ENCRYPTED_FILE_NAME: &str = "secrets.bin";
/// File name remembering which backend was chosen.
pub const PREFERENCE_FILE_NAME: &str = "secret-backend";

/// The backend picked at startup. Implements both [`SecretStore`] and [`SecretStoreControl`]
/// so the composition root can hand the same instance to every use case.
#[derive(Debug)]
pub enum SelectedSecretStore {
    /// The OS vault.
    Keyring(KeyringSecretStore),
    /// The master-password-encrypted file.
    EncryptedFile(EncryptedFileSecretStore),
}

/// Pure selection rule: a remembered choice wins; otherwise use the OS vault if it works.
#[must_use]
pub fn choose_backend(remembered: Option<SecretBackend>, keyring_available: bool) -> SecretBackend {
    remembered.unwrap_or(if keyring_available {
        SecretBackend::Keyring
    } else {
        SecretBackend::EncryptedFile
    })
}

fn parse_backend(text: &str) -> Option<SecretBackend> {
    match text.trim() {
        "keyring" => Some(SecretBackend::Keyring),
        "encrypted_file" => Some(SecretBackend::EncryptedFile),
        _ => None,
    }
}

fn read_preference(data_dir: &Path) -> SecretStoreResult<Option<SecretBackend>> {
    match fs::read_to_string(data_dir.join(PREFERENCE_FILE_NAME)) {
        // An empty file is what an interrupted write of an older build could leave behind;
        // it carries no choice, so it counts as "nothing remembered yet".
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => parse_backend(&text).map(Some).ok_or_else(|| {
            SecretStoreError::Corrupt("unknown secret backend preference".to_owned())
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SecretStoreError::Backend(
            "could not read the secret backend preference".to_owned(),
        )),
    }
}

fn write_preference(data_dir: &Path, backend: SecretBackend) -> SecretStoreResult<()> {
    let io = |_: std::io::Error| {
        SecretStoreError::Backend("could not save the secret backend preference".to_owned())
    };
    fs::create_dir_all(data_dir).map_err(io)?;
    // Write a temp file and rename it over the target so a crash never leaves a truncated
    // preference behind.
    let target = data_dir.join(PREFERENCE_FILE_NAME);
    let temp = data_dir.join(format!("{PREFERENCE_FILE_NAME}.tmp"));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(format!("{}\n", backend.as_str()).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(io)
}

/// Opens the secret store for `data_dir` with the production service name and key-derivation
/// cost.
pub fn open_secret_store(data_dir: &Path) -> SecretStoreResult<SelectedSecretStore> {
    open_secret_store_with(data_dir, KeyringSecretStore::new(), KdfParams::DEFAULT)
}

/// Like [`open_secret_store`] with an explicit vault handle and key-derivation cost.
pub fn open_secret_store_with(
    data_dir: &Path,
    keyring: KeyringSecretStore,
    kdf: KdfParams,
) -> SecretStoreResult<SelectedSecretStore> {
    let remembered = read_preference(data_dir)?;
    // The vault is only probed when no choice was remembered yet.
    let backend = if let Some(backend) = remembered {
        backend
    } else {
        let chosen = choose_backend(None, keyring.is_available());
        write_preference(data_dir, chosen)?;
        chosen
    };
    Ok(match backend {
        SecretBackend::Keyring => SelectedSecretStore::Keyring(keyring),
        SecretBackend::EncryptedFile => SelectedSecretStore::EncryptedFile(
            EncryptedFileSecretStore::with_params(data_dir.join(ENCRYPTED_FILE_NAME), kdf),
        ),
    })
}

impl SecretStore for SelectedSecretStore {
    async fn put(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        match self {
            Self::Keyring(s) => s.put(id, value).await,
            Self::EncryptedFile(s) => s.put(id, value).await,
        }
    }

    async fn get(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        match self {
            Self::Keyring(s) => s.get(id).await,
            Self::EncryptedFile(s) => s.get(id).await,
        }
    }

    async fn delete(&self, id: &SecretId) -> SecretStoreResult<()> {
        match self {
            Self::Keyring(s) => s.delete(id).await,
            Self::EncryptedFile(s) => s.delete(id).await,
        }
    }

    fn is_available(&self) -> bool {
        match self {
            Self::Keyring(s) => s.is_available(),
            Self::EncryptedFile(s) => s.is_available(),
        }
    }
}

impl SecretStoreControl for SelectedSecretStore {
    fn status(&self) -> SecretStoreStatus {
        match self {
            Self::Keyring(s) => s.status(),
            Self::EncryptedFile(s) => s.status(),
        }
    }

    fn unlock(&self, password: &SecretValue) -> SecretStoreResult<()> {
        match self {
            Self::Keyring(s) => s.unlock(password),
            Self::EncryptedFile(s) => s.unlock(password),
        }
    }

    fn set_master_password(
        &self,
        new_password: &SecretValue,
        current_password: Option<&SecretValue>,
    ) -> SecretStoreResult<()> {
        match self {
            Self::Keyring(s) => s.set_master_password(new_password, current_password),
            Self::EncryptedFile(s) => s.set_master_password(new_password, current_password),
        }
    }

    fn lock(&self) {
        match self {
            Self::Keyring(s) => s.lock(),
            Self::EncryptedFile(s) => s.lock(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{choose_backend, parse_backend};
    use tan_application::ports::SecretBackend;

    #[test]
    fn vault_is_preferred_when_it_works_else_the_file() {
        assert_eq!(choose_backend(None, true), SecretBackend::Keyring);
        assert_eq!(choose_backend(None, false), SecretBackend::EncryptedFile);
    }

    #[test]
    fn a_remembered_choice_wins_over_availability() {
        assert_eq!(
            choose_backend(Some(SecretBackend::EncryptedFile), true),
            SecretBackend::EncryptedFile
        );
        assert_eq!(
            choose_backend(Some(SecretBackend::Keyring), false),
            SecretBackend::Keyring
        );
    }

    #[test]
    fn backend_names_round_trip() {
        for backend in [SecretBackend::Keyring, SecretBackend::EncryptedFile] {
            assert_eq!(parse_backend(backend.as_str()), Some(backend));
        }
        assert_eq!(parse_backend("nonsense"), None);
    }
}
