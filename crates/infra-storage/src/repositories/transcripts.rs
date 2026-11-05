//! `TranscriptRepository` over SQLite.

use rusqlite::params;
use tan_application::ports::TranscriptRepository;
use tan_application::RepositoryError;
use tan_domain::{SessionId, TranscriptSegment};

use super::{parse_column, parse_optional_column};
use crate::db::{u64_from_row, u64_to_sql, Database};
use crate::search::{add_doc, remove_session_docs, SOURCE_TRANSCRIPT};

/// SQLite-backed [`TranscriptRepository`].
#[derive(Debug, Clone)]
pub struct SqliteTranscriptRepository {
    db: Database,
}

impl SqliteTranscriptRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl TranscriptRepository for SqliteTranscriptRepository {
    async fn replace_for_session(
        &self,
        session_id: &SessionId,
        segments: &[TranscriptSegment],
    ) -> Result<(), RepositoryError> {
        if let Some(stray) = segments.iter().find(|s| &s.session_id != session_id) {
            return Err(RepositoryError::Invalid(format!(
                "segment belongs to session {}, not {session_id}",
                stray.session_id
            )));
        }
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM transcript_segments WHERE session_id = ?1",
                params![session_id.as_str()],
            )?;
            remove_session_docs(&tx, session_id, SOURCE_TRANSCRIPT)?;
            {
                let mut insert = tx.prepare(
                    "INSERT INTO transcript_segments
                         (session_id, track, start_ms, end_ms, language, text, speaker)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for segment in segments {
                    insert.execute(params![
                        session_id.as_str(),
                        segment.track.as_str(),
                        u64_to_sql(segment.start_ms)?,
                        u64_to_sql(segment.end_ms)?,
                        segment
                            .language
                            .as_ref()
                            .map(tan_domain::LanguageCode::as_str),
                        segment.text,
                        segment.speaker,
                    ])?;
                    let row_id = tx.last_insert_rowid().to_string();
                    add_doc(
                        &tx,
                        session_id,
                        SOURCE_TRANSCRIPT,
                        &row_id,
                        Some(segment.start_ms),
                        &segment.text,
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    async fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<TranscriptSegment>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id, track, start_ms, end_ms, language, text, speaker
                 FROM transcript_segments WHERE session_id = ?1 ORDER BY start_ms, id",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |row| {
                Ok(TranscriptSegment {
                    session_id: SessionId::new(row.get::<_, String>(0)?),
                    track: parse_column(row, 1)?,
                    start_ms: u64_from_row(row, 2)?,
                    end_ms: u64_from_row(row, 3)?,
                    language: parse_optional_column(row, 4)?,
                    text: row.get(5)?,
                    speaker: row.get(6)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}
