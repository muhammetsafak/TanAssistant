//! OS vault secret store (Windows Credential Manager, macOS Keychain, Linux Secret Service).
//!
//! Each secret is one vault entry: service = the application id, user = the secret id.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use keyring::Entry;
use tan_application::ports::{
    SecretBackend, SecretStore, SecretStoreControl, SecretStoreError, SecretStoreResult,
    SecretStoreStatus, SecretValue,
};
use tan_domain::SecretId;

/// Vault service name: the application identifier chosen in phase 01 (`tauri.conf.json`).
pub const SERVICE_NAME: &str = "dev.muhammetsafak.tan";

/// Serialises every call into the OS vault. Concurrent Credential Manager calls from several
/// threads were observed to leave entries behind that had been reported as deleted, so the
/// adapter lets only one call run at a time. It is process-wide (not per store) because the
/// race is in the vault, not in a particular service name.
static VAULT_LOCK: Mutex<()> = Mutex::new(());

fn vault_lock() -> MutexGuard<'static, ()> {
    // The guarded data is `()`: a panic in another thread cannot leave anything inconsistent.
    VAULT_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Account name of the throw-away entry used to probe whether the vault works.
const PROBE_ACCOUNT: &str = "__tan_availability_probe__";

/// [`SecretStore`] over the operating system's vault.
#[derive(Debug)]
pub struct KeyringSecretStore {
    service: String,
    available: OnceLock<bool>,
}

impl KeyringSecretStore {
    /// A store using the production [`SERVICE_NAME`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_service(SERVICE_NAME)
    }

    /// A store under a custom service name (tests use this so they never touch real entries).
    #[must_use]
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            available: OnceLock::new(),
        }
    }

    fn entry(&self, account: &str) -> SecretStoreResult<Entry> {
        Entry::new(&self.service, account).map_err(map_error)
    }

    /// Writes, reads and deletes a probe entry: the only reliable way to learn whether the
    /// vault really works (a Linux box without a Secret Service fails here, not at startup).
    fn probe(&self) -> bool {
        let _guard = vault_lock();
        if Entry::store_status().is_err() {
            return false;
        }
        let Ok(entry) = self.entry(PROBE_ACCOUNT) else {
            return false;
        };
        let works =
            entry.set_password("probe").is_ok() && entry.get_password().is_ok_and(|v| v == "probe");
        let _ = entry.delete_credential();
        works
    }

    fn put_value(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        let _guard = vault_lock();
        self.entry(id.as_str())?
            .set_password(value.expose())
            .map_err(map_error)
    }

    fn get_value(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        let _guard = vault_lock();
        match self.entry(id.as_str())?.get_password() {
            Ok(value) => Ok(Some(SecretValue::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn delete_value(&self, id: &SecretId) -> SecretStoreResult<()> {
        let _guard = vault_lock();
        match self.entry(id.as_str())?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Translates a vault error without ever formatting the error value itself: some variants
/// (`BadEncoding`, `BadDataFormat`) carry the raw stored bytes.
fn map_error(error: keyring::Error) -> SecretStoreError {
    match error {
        keyring::Error::NoStorageAccess(platform) => SecretStoreError::Unavailable(format!(
            "the OS vault is locked or access was denied ({platform})"
        )),
        keyring::Error::NoDefaultStore => {
            SecretStoreError::Unavailable("no OS vault on this platform".to_owned())
        }
        keyring::Error::TooLong(..) => SecretStoreError::ValueTooLong,
        keyring::Error::PlatformFailure(platform) => {
            SecretStoreError::Backend(format!("OS vault failure ({platform})"))
        }
        keyring::Error::BadEncoding(_)
        | keyring::Error::BadDataFormat(..)
        | keyring::Error::BadStoreFormat(_) => {
            SecretStoreError::Corrupt("the vault entry could not be decoded".to_owned())
        }
        _ => SecretStoreError::Backend("OS vault operation failed".to_owned()),
    }
}

impl SecretStore for KeyringSecretStore {
    async fn put(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        self.put_value(id, value)
    }

    async fn get(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        self.get_value(id)
    }

    async fn delete(&self, id: &SecretId) -> SecretStoreResult<()> {
        self.delete_value(id)
    }

    /// Probes the vault once and remembers the answer.
    fn is_available(&self) -> bool {
        *self.available.get_or_init(|| self.probe())
    }
}

impl SecretStoreControl for KeyringSecretStore {
    fn status(&self) -> SecretStoreStatus {
        SecretStoreStatus {
            backend: SecretBackend::Keyring,
            available: self.is_available(),
            locked: false,
            master_password_set: false,
        }
    }

    fn unlock(&self, _password: &SecretValue) -> SecretStoreResult<()> {
        Err(SecretStoreError::Unsupported)
    }

    fn set_master_password(
        &self,
        _new_password: &SecretValue,
        _current_password: Option<&SecretValue>,
    ) -> SecretStoreResult<()> {
        Err(SecretStoreError::Unsupported)
    }

    fn lock(&self) {}
}
