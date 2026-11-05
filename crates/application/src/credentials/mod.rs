//! Credentials use cases: plain variables and secrets (values live in the OS vault, ADR-007).
//!
//! Every error type here is safe to show to the user: none of them ever carries a secret
//! value or a password.

mod secrets;
mod variables;

use std::error::Error;
use std::fmt;

use crate::error::RepositoryError;
use crate::ports::SecretStoreError;

pub use secrets::{
    CreateSecret, CreateSecretInput, DeleteSecret, GetSecretStoreStatus, ListSecrets,
    ResolveSecret, RevealSecret, SecretSummary, SetMasterPassword, UnlockSecretStore, UpdateSecret,
    UpdateSecretInput,
};
pub use variables::{DeleteVariable, ListVariables, UpsertVariable, UpsertVariableInput};

/// Longest variable key, in bytes.
pub const MAX_VARIABLE_KEY_LEN: usize = 128;
/// Longest variable value, in bytes.
pub const MAX_VARIABLE_VALUE_LEN: usize = 8_192;
/// Longest secret name, in bytes.
pub const MAX_SECRET_NAME_LEN: usize = 128;
/// Longest description, in bytes.
pub const MAX_DESCRIPTION_LEN: usize = 1_024;
/// Longest secret value, in bytes. Chosen under the Windows Credential Manager blob limit
/// (2560 bytes, stored as UTF-16) so every backend accepts the same values.
pub const MAX_SECRET_VALUE_LEN: usize = 1_024;
/// Shortest master password, in characters.
pub const MIN_MASTER_PASSWORD_CHARS: usize = 8;
/// Longest master password, in bytes.
pub const MAX_MASTER_PASSWORD_LEN: usize = 256;

/// Why user input was rejected. Never contains the rejected value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A required field is empty.
    Empty(&'static str),
    /// A field exceeds its maximum length (in bytes).
    TooLong {
        /// Field name.
        field: &'static str,
        /// The maximum allowed length.
        max: usize,
    },
    /// A variable key may only contain ASCII letters, digits, `_`, `.` and `-`.
    InvalidKeyCharacters,
    /// A field contains control characters.
    ControlCharacters(&'static str),
    /// The master password is shorter than the minimum.
    PasswordTooShort {
        /// Minimum number of characters.
        min: usize,
    },
    /// An update that changes nothing was requested.
    NothingToUpdate,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty(field) => write!(f, "{field} must not be empty"),
            Self::TooLong { field, max } => write!(f, "{field} is longer than {max} bytes"),
            Self::InvalidKeyCharacters => {
                f.write_str("key may only contain ASCII letters, digits, '_', '.' and '-'")
            }
            Self::ControlCharacters(field) => {
                write!(f, "{field} must not contain control characters")
            }
            Self::PasswordTooShort { min } => {
                write!(f, "the master password needs at least {min} characters")
            }
            Self::NothingToUpdate => f.write_str("the update changes nothing"),
        }
    }
}

impl Error for ValidationError {}

/// Failure of a credentials use case.
#[derive(Debug)]
pub enum CredentialsError {
    /// The input was rejected.
    Invalid(ValidationError),
    /// The addressed entity does not exist.
    NotFound {
        /// Entity name (`variable`, `secret`).
        entity: &'static str,
    },
    /// A uniqueness rule rejected the write; the text names the clashing field.
    Conflict(&'static str),
    /// The secret's metadata exists but the vault holds no value for it.
    SecretValueMissing,
    /// The secret store refused or failed.
    Store(SecretStoreError),
    /// The database failed.
    Storage(RepositoryError),
}

impl fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "invalid input: {e}"),
            Self::NotFound { entity } => write!(f, "{entity} not found"),
            Self::Conflict(what) => f.write_str(what),
            Self::SecretValueMissing => f.write_str("the vault holds no value for this secret"),
            Self::Store(e) => write!(f, "{e}"),
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Storage(_) => f.write_str("the database operation failed"),
        }
    }
}

impl Error for CredentialsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(e) => Some(e),
            Self::Store(e) => Some(e),
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ValidationError> for CredentialsError {
    fn from(e: ValidationError) -> Self {
        Self::Invalid(e)
    }
}

impl From<SecretStoreError> for CredentialsError {
    fn from(e: SecretStoreError) -> Self {
        Self::Store(e)
    }
}

impl CredentialsError {
    /// Classifies a repository failure; `conflict` names what clashed.
    pub(crate) fn from_repository(e: RepositoryError, conflict: &'static str) -> Self {
        match e {
            RepositoryError::NotFound { entity, .. } => Self::NotFound { entity },
            RepositoryError::Conflict(_) => Self::Conflict(conflict),
            other => Self::Storage(other),
        }
    }
}

/// Result alias for credentials use cases.
pub type CredentialsResult<T> = Result<T, CredentialsError>;

/// Trims `text` and checks it is non-empty, within `max` bytes and free of control characters.
pub(crate) fn clean_line(
    field: &'static str,
    text: &str,
    max: usize,
) -> Result<String, ValidationError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ValidationError::Empty(field));
    }
    check_text(field, text, max)?;
    Ok(text.to_owned())
}

/// Checks length and control characters (an empty text passes). Tabs and newlines are fine in
/// free text; other control characters are not.
pub(crate) fn check_text(
    field: &'static str,
    text: &str,
    max: usize,
) -> Result<(), ValidationError> {
    if text.len() > max {
        return Err(ValidationError::TooLong { field, max });
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(ValidationError::ControlCharacters(field));
    }
    Ok(())
}
