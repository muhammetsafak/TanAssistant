//! Use cases for application settings.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use tan_domain::{AppSettings, SettingsPatch};

use crate::error::RepositoryError;
use crate::ports::SettingsRepository;

/// Failure of a settings use case.
#[derive(Debug)]
pub enum SettingsUseCaseError {
    /// A value is outside its allowed range.
    Invalid(String),
    /// The database failed.
    Storage(RepositoryError),
}

impl fmt::Display for SettingsUseCaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "invalid settings: {why}"),
            // Details stay in `source()` for logs.
            Self::Storage(_) => f.write_str("the database operation failed"),
        }
    }
}

impl Error for SettingsUseCaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::Storage(e) => Some(e),
        }
    }
}

impl From<RepositoryError> for SettingsUseCaseError {
    fn from(e: RepositoryError) -> Self {
        match e {
            RepositoryError::Invalid(why) => Self::Invalid(why),
            other => Self::Storage(other),
        }
    }
}

/// Reads the current settings.
pub struct GetSettings<R> {
    settings: Arc<R>,
}

impl<R: SettingsRepository> GetSettings<R> {
    /// Creates the use case.
    #[must_use]
    pub fn new(settings: Arc<R>) -> Self {
        Self { settings }
    }

    /// Runs it; never-written fields carry the domain defaults.
    pub async fn execute(&self) -> Result<AppSettings, SettingsUseCaseError> {
        Ok(self.settings.load().await?)
    }
}

/// Applies a partial settings update.
pub struct UpdateSettings<R> {
    settings: Arc<R>,
}

impl<R: SettingsRepository> UpdateSettings<R> {
    /// Creates the use case.
    #[must_use]
    pub fn new(settings: Arc<R>) -> Self {
        Self { settings }
    }

    /// Validates (through the domain) and stores the patch; returns the resulting settings.
    pub async fn execute(
        &self,
        patch: &SettingsPatch,
    ) -> Result<AppSettings, SettingsUseCaseError> {
        Ok(self.settings.update(patch).await?)
    }
}
