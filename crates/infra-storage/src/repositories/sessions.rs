//! `SessionRepository` over SQLite.

use rusqlite::{params, OptionalExtension, Row};
use tan_application::ports::SessionRepository;
use tan_application::RepositoryError;
use tan_domain::{AudioTrack, EpochMs, Session, SessionId, SessionRecord, TrackKind, TrackMute};

use super::{parse_column, require_changed};
use crate::db::{u64_from_row, u64_to_sql, Database};

const SESSION_COLUMNS: &str =
    "id, kind, title, started_at, ended_at, language, note_language, status";

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    let record = SessionRecord {
        id: SessionId::new(row.get::<_, String>(0)?),
        kind: parse_column(row, 1)?,
        title: row.get(2)?,
        started_at: EpochMs::new(row.get(3)?),
        ended_at: row.get::<_, Option<i64>>(4)?.map(EpochMs::new),
        language: parse_column(row, 5)?,
        note_language: parse_column(row, 6)?,
        status: parse_column(row, 7)?,
    };
    Session::restore(record).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Integer, Box::new(e))
    })
}

/// SQLite-backed [`SessionRepository`].
#[derive(Debug, Clone)]
pub struct SqliteSessionRepository {
    db: Database,
}

impl SqliteSessionRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl SessionRepository for SqliteSessionRepository {
    async fn create(&self, session: &Session) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, kind, title, started_at, ended_at, language,
                                       note_language, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session.id.as_str(),
                    session.kind.as_str(),
                    session.title,
                    session.started_at.get(),
                    session.ended_at().map(EpochMs::get),
                    session.language.to_storage_string(),
                    session.note_language.as_str(),
                    session.status().as_str(),
                ],
            )?;
            Ok(())
        })
    }

    async fn update(&self, session: &Session) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE sessions
                 SET kind = ?2, title = ?3, started_at = ?4, ended_at = ?5, language = ?6,
                     note_language = ?7, status = ?8
                 WHERE id = ?1",
                params![
                    session.id.as_str(),
                    session.kind.as_str(),
                    session.title,
                    session.started_at.get(),
                    session.ended_at().map(EpochMs::get),
                    session.language.to_storage_string(),
                    session.note_language.as_str(),
                    session.status().as_str(),
                ],
            )?;
            require_changed(changed, "session", session.id.as_str())
        })
    }

    async fn get(&self, id: &SessionId) -> Result<Option<Session>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                    params![id.as_str()],
                    session_from_row,
                )
                .optional()?)
        })
    }

    async fn list(&self) -> Result<Vec<Session>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY started_at DESC, id DESC"
            ))?;
            let rows = stmt.query_map([], session_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn delete(&self, id: &SessionId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed =
                conn.execute("DELETE FROM sessions WHERE id = ?1", params![id.as_str()])?;
            require_changed(changed, "session", id.as_str())
        })
    }

    async fn save_audio_track(&self, track: &AudioTrack) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO audio_tracks (session_id, kind, path, sample_rate, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (session_id, kind) DO UPDATE SET
                     path = excluded.path,
                     sample_rate = excluded.sample_rate,
                     duration_ms = excluded.duration_ms",
                params![
                    track.session_id.as_str(),
                    track.kind.as_str(),
                    track.path,
                    track.sample_rate,
                    u64_to_sql(track.duration_ms)?,
                ],
            )?;
            Ok(())
        })
    }

    async fn list_audio_tracks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AudioTrack>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id, kind, path, sample_rate, duration_ms
                 FROM audio_tracks WHERE session_id = ?1 ORDER BY kind",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |row| {
                Ok(AudioTrack {
                    session_id: SessionId::new(row.get::<_, String>(0)?),
                    kind: parse_column(row, 1)?,
                    path: row.get(2)?,
                    sample_rate: row.get(3)?,
                    duration_ms: u64_from_row(row, 4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn open_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        start_ms: u64,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO track_mutes (session_id, kind, start_ms, end_ms)
                 VALUES (?1, ?2, ?3, NULL)",
                params![session_id.as_str(), kind.as_str(), u64_to_sql(start_ms)?],
            )?;
            Ok(())
        })
    }

    async fn close_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        end_ms: u64,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            // An end before the start would break the table's check; clamp it instead.
            conn.execute(
                "UPDATE track_mutes SET end_ms = MAX(?3, start_ms)
                 WHERE session_id = ?1 AND kind = ?2 AND end_ms IS NULL",
                params![session_id.as_str(), kind.as_str(), u64_to_sql(end_ms)?],
            )?;
            Ok(())
        })
    }

    async fn list_track_mutes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<TrackMute>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id, kind, start_ms, end_ms FROM track_mutes
                 WHERE session_id = ?1 ORDER BY start_ms, id",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |row| {
                let end_ms = match row.get::<_, Option<i64>>(3)? {
                    Some(value) => Some(
                        u64::try_from(value)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, value))?,
                    ),
                    None => None,
                };
                Ok(TrackMute {
                    session_id: SessionId::new(row.get::<_, String>(0)?),
                    kind: parse_column(row, 1)?,
                    start_ms: u64_from_row(row, 2)?,
                    end_ms,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}
