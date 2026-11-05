//! Secret value storage ports (ADR-007).
//!
//! Secret **values** only ever travel through [`SecretStore`]; the database keeps metadata
//! only. [`SecretValue`] makes accidental leaks hard: it prints as redacted, has no
//! `Serialize`, and wipes its memory when dropped.

use std::error::Error;
use std::fmt;
use std::future::Future;

use tan_domain::SecretId;
use zeroize::Zeroize;

/// A secret value (API key, password...). Redacted in `Debug`/`Display`, zeroed on drop,
/// deliberately not `Serialize` and not `Clone`.
pub struct SecretValue(String);

impl SecretValue {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Call this only at the point of use (sending it to a provider, writing
    /// it to the vault, handing it to the user on an explicit reveal).
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Failure of a secret store operation. No variant ever carries a secret value or a password.
#[derive(Debug)]
pub enum SecretStoreError {
    /// The backend needs a master password and none has been set yet.
    NotConfigured,
    /// The backend is locked; unlock it with the master password first.
    Locked,
    /// The master password is wrong.
    InvalidPassword,
    /// The operation does not apply to this backend (e.g. a master password on the OS vault).
    Unsupported,
    /// The backend cannot work in this environment (no OS vault, no data directory...).
    Unavailable(String),
    /// The platform vault refuses a value this long.
    ValueTooLong,
    /// Stored data is malformed or was tampered with.
    Corrupt(String),
    /// Any other backend failure. The message never contains values.
    Backend(String),
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("the secret store has no master password yet"),
            Self::Locked => f.write_str("the secret store is locked"),
            Self::InvalidPassword => f.write_str("the master password is wrong"),
            Self::Unsupported => f.write_str("operation not supported by this secret store"),
            Self::Unavailable(why) => write!(f, "the secret store is unavailable: {why}"),
            Self::ValueTooLong => f.write_str("the secret value is too long for the secret store"),
            Self::Corrupt(why) => write!(f, "the secret store data is corrupt: {why}"),
            Self::Backend(why) => write!(f, "secret store failure: {why}"),
        }
    }
}

impl Error for SecretStoreError {}

/// Result alias for secret store operations.
pub type SecretStoreResult<T> = Result<T, SecretStoreError>;

/// Where secret values live.
pub trait SecretStore: Send + Sync {
    /// Stores (or replaces) the value of `id`.
    fn put(
        &self,
        id: &SecretId,
        value: &SecretValue,
    ) -> impl Future<Output = SecretStoreResult<()>> + Send;

    /// The value of `id`, or `None` if nothing is stored under it.
    fn get(
        &self,
        id: &SecretId,
    ) -> impl Future<Output = SecretStoreResult<Option<SecretValue>>> + Send;

    /// Removes the value of `id`. Removing a missing value is not an error.
    fn delete(&self, id: &SecretId) -> impl Future<Output = SecretStoreResult<()>> + Send;

    /// Whether this backend can work in the current environment.
    fn is_available(&self) -> bool;
}

/// Which backend holds the secret values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    /// The operating system's vault (Credential Manager, Keychain, Secret Service).
    Keyring,
    /// A master-password-encrypted file in the data directory.
    EncryptedFile,
}

impl SecretBackend {
    /// Stable textual form (used for persistence and the IPC layer).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::EncryptedFile => "encrypted_file",
        }
    }
}

/// Snapshot of the secret store's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretStoreStatus {
    /// The active backend.
    pub backend: SecretBackend,
    /// Whether the backend can work in this environment.
    pub available: bool,
    /// Whether values are unreachable until the master password is entered.
    pub locked: bool,
    /// Whether a master password exists (always `false` for the OS vault).
    pub master_password_set: bool,
}

/// Lock/unlock control of the secret store. These calls can be slow (key derivation), so
/// they are synchronous: run them on a blocking thread.
pub trait SecretStoreControl: Send + Sync {
    /// Current state.
    fn status(&self) -> SecretStoreStatus;

    /// Unlocks the store with the master password.
    fn unlock(&self, password: &SecretValue) -> SecretStoreResult<()>;

    /// Sets the master password the first time, or changes it (then `current_password` is
    /// required). Leaves the store unlocked.
    fn set_master_password(
        &self,
        new_password: &SecretValue,
        current_password: Option<&SecretValue>,
    ) -> SecretStoreResult<()>;

    /// Locks the store again.
    fn lock(&self);
}

#[cfg(test)]
mod tests {
    use super::SecretValue;

    #[test]
    fn debug_and_display_never_show_the_value() {
        let value = SecretValue::new("sk-live-0123456789");
        assert!(!format!("{value:?}").contains("sk-live"));
        assert!(!format!("{value}").contains("sk-live"));
        assert!(!format!("{:#?}", Some(&value)).contains("sk-live"));
        assert_eq!(value.expose(), "sk-live-0123456789");
    }
}
