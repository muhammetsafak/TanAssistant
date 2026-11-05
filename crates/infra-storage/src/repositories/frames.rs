//! `FrameRepository` over SQLite.

use rusqlite::{params, OptionalExtension, Row};
use tan_application::ports::FrameRepository;
use tan_application::RepositoryError;
use tan_domain::{Frame, FrameId, SessionId};

use super::{hash_from_sql, hash_to_sql, require_changed};
use crate::db::{u64_from_row, u64_to_sql, Database};

const FRAME_COLUMNS: &str = "id, session_id, timestamp_ms, path, perceptual_hash, description";

fn frame_from_row(row: &Row<'_>) -> rusqlite::Result<Frame> {
    Ok(Frame {
        id: FrameId::new(row.get::<_, String>(0)?),
        session_id: SessionId::new(row.get::<_, String>(1)?),
        timestamp_ms: u64_from_row(row, 2)?,
        path: row.get(3)?,
        perceptual_hash: hash_from_sql(row.get(4)?),
        description: row.get(5)?,
    })
}

/// SQLite-backed [`FrameRepository`].
#[derive(Debug, Clone)]
pub struct SqliteFrameRepository {
    db: Database,
}

impl SqliteFrameRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl FrameRepository for SqliteFrameRepository {
    async fn add(&self, frame: &Frame) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO frames (id, session_id, timestamp_ms, path, perceptual_hash,
                                     description)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    frame.id.as_str(),
                    frame.session_id.as_str(),
                    u64_to_sql(frame.timestamp_ms)?,
                    frame.path,
                    hash_to_sql(frame.perceptual_hash),
                    frame.description,
                ],
            )?;
            Ok(())
        })
    }

    async fn get(&self, id: &FrameId) -> Result<Option<Frame>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {FRAME_COLUMNS} FROM frames WHERE id = ?1"),
                    params![id.as_str()],
                    frame_from_row,
                )
                .optional()?)
        })
    }

    async fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<Frame>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FRAME_COLUMNS} FROM frames WHERE session_id = ?1
                 ORDER BY timestamp_ms, id"
            ))?;
            let rows = stmt.query_map(params![session_id.as_str()], frame_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn set_description(
        &self,
        id: &FrameId,
        description: Option<&str>,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE frames SET description = ?2 WHERE id = ?1",
                params![id.as_str(), description],
            )?;
            require_changed(changed, "frame", id.as_str())
        })
    }

    async fn delete(&self, id: &FrameId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM frames WHERE id = ?1", params![id.as_str()])?;
            require_changed(changed, "frame", id.as_str())
        })
    }
}
