//! `SecretMetadataRepository` over SQLite. Stores metadata only; values live in the OS vault
//! (ADR-007).

use rusqlite::{params, OptionalExtension, Row};
use tan_application::ports::SecretMetadataRepository;
use tan_application::RepositoryError;
use tan_domain::{EpochMs, SecretId, SecretRef};

use super::require_changed;
use crate::db::Database;

const SECRET_COLUMNS: &str = "id, name, description, created_at";

fn secret_from_row(row: &Row<'_>) -> rusqlite::Result<SecretRef> {
    Ok(SecretRef {
        id: SecretId::new(row.get::<_, String>(0)?),
        name: row.get(1)?,
        description: row.get(2)?,
        created_at: EpochMs::new(row.get(3)?),
    })
}

/// SQLite-backed [`SecretMetadataRepository`].
#[derive(Debug, Clone)]
pub struct SqliteSecretMetadataRepository {
    db: Database,
}

impl SqliteSecretMetadataRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl SecretMetadataRepository for SqliteSecretMetadataRepository {
    async fn create(&self, secret: &SecretRef) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO secrets (id, name, description, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![
                    secret.id.as_str(),
                    secret.name,
                    secret.description,
                    secret.created_at.get()
                ],
            )?;
            Ok(())
        })
    }

    async fn update(&self, secret: &SecretRef) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE secrets SET name = ?2, description = ?3 WHERE id = ?1",
                params![secret.id.as_str(), secret.name, secret.description],
            )?;
            require_changed(changed, "secret", secret.id.as_str())
        })
    }

    async fn get(&self, id: &SecretId) -> Result<Option<SecretRef>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {SECRET_COLUMNS} FROM secrets WHERE id = ?1"),
                    params![id.as_str()],
                    secret_from_row,
                )
                .optional()?)
        })
    }

    async fn list(&self) -> Result<Vec<SecretRef>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SECRET_COLUMNS} FROM secrets ORDER BY name"
            ))?;
            let rows = stmt.query_map([], secret_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn delete(&self, id: &SecretId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed =
                conn.execute("DELETE FROM secrets WHERE id = ?1", params![id.as_str()])?;
            require_changed(changed, "secret", id.as_str())
        })
    }
}
