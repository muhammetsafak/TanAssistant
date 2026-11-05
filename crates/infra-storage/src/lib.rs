//! Storage adapter: SQLite repositories and the data directory.
//!
//! Wire-up: build a [`DataLayout`], open a [`Database`] at `layout.database_path()`, then hand
//! clones of the `Database` to the `Sqlite*Repository` types (and [`SqliteSearchIndex`]).

// The port traits are async-shaped, but SQLite calls here are short synchronous statements,
// so the trait impls have no `.await`. Wrapping each in `std::future::ready` would only add
// noise; keep `async fn` so the impls stay valid if a call ever becomes genuinely async.
#![allow(clippy::unused_async_trait_impl)]

pub mod db;
pub mod layout;
pub mod repositories;
pub mod search;

pub use db::{Database, StorageError, LATEST_SCHEMA_VERSION};
pub use layout::DataLayout;
pub use repositories::{
    SqliteFrameRepository, SqliteNoteRepository, SqliteProviderProfileRepository,
    SqliteSecretMetadataRepository, SqliteSessionRepository, SqliteSettingsRepository,
    SqliteTaskAssignmentRepository, SqliteTranscriptRepository, SqliteVariableRepository,
};
pub use search::SqliteSearchIndex;

/// Mints a new time-ordered (UUID v7) identifier, suitable for any `tan_domain` id type.
#[must_use]
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-storage";

#[cfg(test)]
mod tests {
    use super::{new_id, LAYER};

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-storage");
    }

    #[test]
    fn minted_ids_are_unique_and_time_ordered() {
        let first = new_id();
        let second = new_id();
        assert_ne!(first, second);
        assert!(first < second, "UUID v7 sorts by creation time");
    }
}
