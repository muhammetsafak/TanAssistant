//! SQLite implementations of the repository ports.

mod frames;
mod notes;
mod processing;
mod providers;
mod secrets;
mod sessions;
mod settings;
mod transcripts;
mod variables;

use std::error::Error;
use std::str::FromStr;

use rusqlite::types::Type;
use rusqlite::Row;
use tan_application::RepositoryError;

use crate::db::DbError;

pub use frames::SqliteFrameRepository;
pub use notes::SqliteNoteRepository;
pub use processing::{DataDirFiles, SqliteProcessingRepository};
pub use providers::{SqliteProviderProfileRepository, SqliteTaskAssignmentRepository};
pub use secrets::SqliteSecretMetadataRepository;
pub use sessions::SqliteSessionRepository;
pub use settings::SqliteSettingsRepository;
pub use transcripts::SqliteTranscriptRepository;
pub use variables::SqliteVariableRepository;

/// Reads column `idx` as text and parses it, reporting a parse failure as a conversion error
/// (which surfaces as [`RepositoryError::CorruptData`]).
fn parse_column<T>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let text: String = row.get(idx)?;
    text.parse().map_err(|e: T::Err| {
        rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
    })
}

/// Like [`parse_column`] for nullable columns.
fn parse_optional_column<T>(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let text: Option<String> = row.get(idx)?;
    text.map(|t| {
        t.parse().map_err(|e: T::Err| {
            rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
        })
    })
    .transpose()
}

/// Maps "zero rows affected" to [`RepositoryError::NotFound`].
fn require_changed(changed: usize, entity: &'static str, id: &str) -> Result<(), DbError> {
    if changed == 0 {
        Err(RepositoryError::not_found(entity, id).into())
    } else {
        Ok(())
    }
}

/// Stores a `u64` perceptual hash bit-for-bit in SQLite's signed integer.
fn hash_to_sql(hash: u64) -> i64 {
    i64::from_ne_bytes(hash.to_ne_bytes())
}

/// Inverse of [`hash_to_sql`].
fn hash_from_sql(stored: i64) -> u64 {
    u64::from_ne_bytes(stored.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::{hash_from_sql, hash_to_sql};

    #[test]
    fn perceptual_hash_survives_the_signed_round_trip() {
        for hash in [0, 1, u64::MAX, 1 << 63, 0xDEAD_BEEF_CAFE_F00D] {
            assert_eq!(hash_from_sql(hash_to_sql(hash)), hash);
        }
    }
}
