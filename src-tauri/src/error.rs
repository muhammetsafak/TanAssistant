//! The error type every IPC command returns.
//!
//! The message is derived from use-case errors that are guaranteed value-free (ADR-007): it
//! never contains a secret value or a password. Backend details are logged, not returned.

use std::error::Error;

use serde::Serialize;
use specta::Type;
use tan_application::credentials::CredentialsError;
use tan_application::ports::SecretStoreError;
use tan_application::settings::SettingsUseCaseError;

/// Machine-readable failure class, so the UI can react without parsing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The input was rejected.
    Validation,
    /// The addressed entity does not exist.
    NotFound,
    /// A uniqueness rule rejected the write.
    Conflict,
    /// The secret store is locked; unlock it with the master password.
    SecretStoreLocked,
    /// The secret store needs a master password to be set first.
    SecretStoreNotConfigured,
    /// The master password is wrong.
    InvalidPassword,
    /// The secret store cannot work in this environment.
    SecretStoreUnavailable,
    /// The vault holds no value for a secret that exists in the database.
    SecretValueMissing,
    /// Anything else; details are in the log.
    Internal,
}

/// Error returned to the UI.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    /// Failure class.
    pub code: ErrorCode,
    /// Human-readable, value-free description (English; the UI localises by `code`).
    pub message: String,
}

impl IpcError {
    /// Builds an error from its parts.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// A validation failure.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }
}

fn log_source(context: &str, error: &(dyn Error + 'static)) {
    // Walk the chain by hand so only `Display` strings are logged.
    let mut chain = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push(cause.to_string());
        source = cause.source();
    }
    tracing::error!(error = %chain.join(": "), "{context}");
}

impl From<CredentialsError> for IpcError {
    fn from(error: CredentialsError) -> Self {
        let code = match &error {
            CredentialsError::Invalid(_) => ErrorCode::Validation,
            CredentialsError::NotFound { .. } => ErrorCode::NotFound,
            CredentialsError::Conflict(_) => ErrorCode::Conflict,
            CredentialsError::SecretValueMissing => ErrorCode::SecretValueMissing,
            CredentialsError::Store(store) => match store {
                SecretStoreError::Locked => ErrorCode::SecretStoreLocked,
                SecretStoreError::NotConfigured => ErrorCode::SecretStoreNotConfigured,
                SecretStoreError::InvalidPassword => ErrorCode::InvalidPassword,
                SecretStoreError::Unavailable(_) | SecretStoreError::Unsupported => {
                    ErrorCode::SecretStoreUnavailable
                }
                SecretStoreError::ValueTooLong => ErrorCode::Validation,
                SecretStoreError::Corrupt(_) | SecretStoreError::Backend(_) => ErrorCode::Internal,
            },
            CredentialsError::Storage(_) => ErrorCode::Internal,
        };
        if code == ErrorCode::Internal {
            log_source("credentials operation failed", &error);
        }
        Self::new(code, error.to_string())
    }
}

impl From<SettingsUseCaseError> for IpcError {
    fn from(error: SettingsUseCaseError) -> Self {
        let code = match &error {
            SettingsUseCaseError::Invalid(_) => ErrorCode::Validation,
            SettingsUseCaseError::Storage(_) => {
                log_source("settings operation failed", &error);
                ErrorCode::Internal
            }
        };
        Self::new(code, error.to_string())
    }
}
