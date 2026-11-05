//! AI use cases: provider profiles, connection tests and task assignments (ADR-008).
//!
//! Every error type here is safe to show to the user: none carries an API key.

mod assignments;
mod profiles;

use std::error::Error;
use std::fmt;

use tan_domain::{ProviderKind, TaskKind};

use crate::credentials::ValidationError;
use crate::error::RepositoryError;
use crate::ports::AiError;

pub use assignments::{ClearTaskAssignment, GetTaskAssignments, SetTaskAssignment};
pub use profiles::{
    DeleteProviderProfile, ListProviderProfiles, ProviderTestReport, TestProvider,
    UpsertProviderProfile, UpsertProviderProfileInput,
};

/// Longest profile display name, in bytes.
pub const MAX_DISPLAY_NAME_LEN: usize = 128;
/// Longest model identifier, in bytes.
pub const MAX_MODEL_LEN: usize = 200;
/// Longest base URL, in bytes.
pub const MAX_BASE_URL_LEN: usize = 2_048;

/// Failure of an AI use case.
#[derive(Debug)]
pub enum AiUseCaseError {
    /// The input was rejected; the text says why and never contains the rejected secret.
    Invalid(String),
    /// The addressed entity does not exist.
    NotFound {
        /// Entity name (`provider profile`, `secret`).
        entity: &'static str,
    },
    /// A reference rule rejected the write.
    Conflict(&'static str),
    /// The provider cannot perform the task (e.g. `DeepSeek` for vision).
    CapabilityMissing {
        /// The task that was requested.
        task: TaskKind,
        /// The provider that lacks the capability.
        provider: ProviderKind,
    },
    /// The provider call failed.
    Provider(AiError),
    /// The database failed.
    Storage(RepositoryError),
}

impl fmt::Display for AiUseCaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "invalid input: {why}"),
            Self::NotFound { entity } => write!(f, "{entity} not found"),
            Self::Conflict(what) => f.write_str(what),
            Self::CapabilityMissing { task, provider } => {
                write!(f, "{provider} cannot be used for the {task} task")
            }
            Self::Provider(e) => write!(f, "{e}"),
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Storage(_) => f.write_str("the database operation failed"),
        }
    }
}

impl Error for AiUseCaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(e) => Some(e),
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ValidationError> for AiUseCaseError {
    fn from(e: ValidationError) -> Self {
        Self::Invalid(e.to_string())
    }
}

impl From<AiError> for AiUseCaseError {
    fn from(e: AiError) -> Self {
        Self::Provider(e)
    }
}

impl AiUseCaseError {
    /// Classifies a repository failure; `conflict` names what clashed.
    pub(crate) fn from_repository(e: RepositoryError, conflict: &'static str) -> Self {
        match e {
            RepositoryError::NotFound { entity, .. } => Self::NotFound { entity },
            RepositoryError::Conflict(_) => Self::Conflict(conflict),
            RepositoryError::Invalid(why) => Self::Invalid(why),
            other => Self::Storage(other),
        }
    }
}

/// Result alias for AI use cases.
pub type AiUseCaseResult<T> = Result<T, AiUseCaseError>;
