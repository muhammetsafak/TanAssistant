//! `VariableRepository` over SQLite.

use rusqlite::{params, OptionalExtension, Row};
use tan_application::ports::VariableRepository;
use tan_application::RepositoryError;
use tan_domain::{Variable, VariableId};

use super::require_changed;
use crate::db::Database;

const VARIABLE_COLUMNS: &str = "id, key, value, description";

fn variable_from_row(row: &Row<'_>) -> rusqlite::Result<Variable> {
    Ok(Variable {
        id: VariableId::new(row.get::<_, String>(0)?),
        key: row.get(1)?,
        value: row.get(2)?,
        description: row.get(3)?,
    })
}

/// SQLite-backed [`VariableRepository`].
#[derive(Debug, Clone)]
pub struct SqliteVariableRepository {
    db: Database,
}

impl SqliteVariableRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl VariableRepository for SqliteVariableRepository {
    async fn upsert(&self, variable: &Variable) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO variables (id, key, value, description) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                     key = excluded.key,
                     value = excluded.value,
                     description = excluded.description",
                params![
                    variable.id.as_str(),
                    variable.key,
                    variable.value,
                    variable.description
                ],
            )?;
            Ok(())
        })
    }

    async fn get(&self, id: &VariableId) -> Result<Option<Variable>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {VARIABLE_COLUMNS} FROM variables WHERE id = ?1"),
                    params![id.as_str()],
                    variable_from_row,
                )
                .optional()?)
        })
    }

    async fn get_by_key(&self, key: &str) -> Result<Option<Variable>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {VARIABLE_COLUMNS} FROM variables WHERE key = ?1"),
                    params![key],
                    variable_from_row,
                )
                .optional()?)
        })
    }

    async fn list(&self) -> Result<Vec<Variable>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {VARIABLE_COLUMNS} FROM variables ORDER BY key"
            ))?;
            let rows = stmt.query_map([], variable_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn delete(&self, id: &VariableId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed =
                conn.execute("DELETE FROM variables WHERE id = ?1", params![id.as_str()])?;
            require_changed(changed, "variable", id.as_str())
        })
    }
}
