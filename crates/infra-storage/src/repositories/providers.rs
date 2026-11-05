//! `ProviderProfileRepository` and `TaskAssignmentRepository` over SQLite.

use rusqlite::{params, OptionalExtension, Row};
use tan_application::ports::{ProviderProfileRepository, TaskAssignmentRepository};
use tan_application::RepositoryError;
use tan_domain::{ProviderProfile, ProviderProfileId, SecretId, TaskAssignment, TaskKind};

use super::{parse_column, require_changed};
use crate::db::Database;

const PROFILE_COLUMNS: &str = "id, kind, display_name, model, base_url, secret_ref";

fn profile_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderProfile> {
    Ok(ProviderProfile {
        id: ProviderProfileId::new(row.get::<_, String>(0)?),
        kind: parse_column(row, 1)?,
        display_name: row.get(2)?,
        model: row.get(3)?,
        base_url: row.get(4)?,
        secret_ref: row.get::<_, Option<String>>(5)?.map(SecretId::new),
    })
}

/// SQLite-backed [`ProviderProfileRepository`].
#[derive(Debug, Clone)]
pub struct SqliteProviderProfileRepository {
    db: Database,
}

impl SqliteProviderProfileRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl ProviderProfileRepository for SqliteProviderProfileRepository {
    async fn upsert(&self, profile: &ProviderProfile) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO provider_profiles (id, kind, display_name, model, base_url, secret_ref)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     kind = excluded.kind,
                     display_name = excluded.display_name,
                     model = excluded.model,
                     base_url = excluded.base_url,
                     secret_ref = excluded.secret_ref",
                params![
                    profile.id.as_str(),
                    profile.kind.as_str(),
                    profile.display_name,
                    profile.model,
                    profile.base_url,
                    profile.secret_ref.as_ref().map(SecretId::as_str),
                ],
            )?;
            Ok(())
        })
    }

    async fn get(
        &self,
        id: &ProviderProfileId,
    ) -> Result<Option<ProviderProfile>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {PROFILE_COLUMNS} FROM provider_profiles WHERE id = ?1"),
                    params![id.as_str()],
                    profile_from_row,
                )
                .optional()?)
        })
    }

    async fn list(&self) -> Result<Vec<ProviderProfile>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PROFILE_COLUMNS} FROM provider_profiles ORDER BY display_name, id"
            ))?;
            let rows = stmt.query_map([], profile_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn delete(&self, id: &ProviderProfileId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "DELETE FROM provider_profiles WHERE id = ?1",
                params![id.as_str()],
            )?;
            require_changed(changed, "provider profile", id.as_str())
        })
    }
}

/// SQLite-backed [`TaskAssignmentRepository`].
#[derive(Debug, Clone)]
pub struct SqliteTaskAssignmentRepository {
    db: Database,
}

impl SqliteTaskAssignmentRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

fn assignment_from_row(row: &Row<'_>) -> rusqlite::Result<TaskAssignment> {
    Ok(TaskAssignment {
        task: parse_column(row, 0)?,
        profile_id: ProviderProfileId::new(row.get::<_, String>(1)?),
    })
}

impl TaskAssignmentRepository for SqliteTaskAssignmentRepository {
    async fn set(&self, assignment: &TaskAssignment) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO task_assignments (task, profile_id) VALUES (?1, ?2)
                 ON CONFLICT (task) DO UPDATE SET profile_id = excluded.profile_id",
                params![assignment.task.as_str(), assignment.profile_id.as_str()],
            )?;
            Ok(())
        })
    }

    async fn get(&self, task: TaskKind) -> Result<Option<TaskAssignment>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT task, profile_id FROM task_assignments WHERE task = ?1",
                    params![task.as_str()],
                    assignment_from_row,
                )
                .optional()?)
        })
    }

    async fn list(&self) -> Result<Vec<TaskAssignment>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT task, profile_id FROM task_assignments ORDER BY task")?;
            let rows = stmt.query_map([], assignment_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn clear(&self, task: TaskKind) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM task_assignments WHERE task = ?1",
                params![task.as_str()],
            )?;
            Ok(())
        })
    }
}
