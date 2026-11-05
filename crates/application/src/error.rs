//! Errors shared by the application ports.

use std::error::Error;
use std::fmt;

/// Failure of a repository operation, independent of the storage technology behind it.
#[derive(Debug)]
pub enum RepositoryError {
    /// The addressed entity does not exist.
    NotFound {
        /// Entity name (e.g. `session`).
        entity: &'static str,
        /// Identifier that was looked up.
        id: String,
    },
    /// A uniqueness or integrity rule rejected the write (duplicate id/name, dangling reference).
    Conflict(String),
    /// The request itself is invalid (for example a settings value out of range).
    Invalid(String),
    /// Stored data could not be mapped back to a valid domain value.
    CorruptData(String),
    /// The storage backend failed (I/O, connection, migration...).
    Storage(Box<dyn Error + Send + Sync>),
}

impl RepositoryError {
    /// Convenience constructor for [`RepositoryError::NotFound`].
    #[must_use]
    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }

    /// Wraps a backend error as [`RepositoryError::Storage`].
    pub fn storage(source: impl Error + Send + Sync + 'static) -> Self {
        Self::Storage(Box::new(source))
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::Invalid(msg) => write!(f, "invalid request: {msg}"),
            Self::CorruptData(msg) => write!(f, "corrupt stored data: {msg}"),
            Self::Storage(err) => write!(f, "storage error: {err}"),
        }
    }
}

impl Error for RepositoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

/// Result alias for repository operations.
pub type RepositoryResult<T> = Result<T, RepositoryError>;

#[cfg(test)]
mod tests {
    use super::RepositoryError;

    #[test]
    fn display_names_entity_and_id() {
        let err = RepositoryError::not_found("session", "abc");
        assert_eq!(err.to_string(), "session not found: abc");
    }

    #[test]
    fn storage_error_exposes_its_source() {
        use std::error::Error;
        let err = RepositoryError::storage(std::io::Error::other("disk gone"));
        assert!(err.source().is_some());
        assert!(err.to_string().contains("disk gone"));
    }
}
