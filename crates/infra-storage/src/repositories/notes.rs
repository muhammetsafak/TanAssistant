//! `NoteRepository` over SQLite.

use rusqlite::{params, Connection, OptionalExtension, Row};
use tan_application::ports::NoteRepository;
use tan_application::RepositoryError;
use tan_domain::{ActionItem, ActionItemId, EpochMs, FrameId, Note, NoteId, SessionId};

use super::{parse_column, require_changed};
use crate::db::{Database, DbError};
use crate::search::{add_doc, remove_ref_docs, SOURCE_NOTE};

const KIND_DECISION: &str = "decision";
const KIND_OPEN_QUESTION: &str = "open_question";

const NOTE_COLUMNS: &str = "id, session_id, language, summary, created_at";

/// Reads the ordered texts of one kind (`decision` / `open_question`) of a note.
fn note_texts(conn: &Connection, note_id: &str, kind: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT text FROM note_items WHERE note_id = ?1 AND kind = ?2 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![note_id, kind], |row| row.get(0))?;
    rows.collect()
}

/// Loads a full note (header row plus its ordered child rows).
fn load_note(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<Note>, DbError> {
    let header = conn
        .query_row(sql, args, |row: &Row<'_>| {
            Ok((
                NoteId::new(row.get::<_, String>(0)?),
                SessionId::new(row.get::<_, String>(1)?),
                parse_column(row, 2)?,
                row.get::<_, String>(3)?,
                EpochMs::new(row.get(4)?),
            ))
        })
        .optional()?;
    let Some((id, session_id, language, summary, created_at)) = header else {
        return Ok(None);
    };
    let decisions = note_texts(conn, id.as_str(), KIND_DECISION)?;
    let open_questions = note_texts(conn, id.as_str(), KIND_OPEN_QUESTION)?;
    let mut stmt =
        conn.prepare("SELECT frame_id FROM note_frame_refs WHERE note_id = ?1 ORDER BY position")?;
    let frame_refs = stmt
        .query_map(params![id.as_str()], |row| {
            Ok(FrameId::new(row.get::<_, String>(0)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Note {
        id,
        session_id,
        language,
        summary,
        decisions,
        open_questions,
        frame_refs,
        created_at,
    }))
}

fn action_item_from_row(row: &Row<'_>) -> rusqlite::Result<ActionItem> {
    Ok(ActionItem {
        id: ActionItemId::new(row.get::<_, String>(0)?),
        note_id: NoteId::new(row.get::<_, String>(1)?),
        task: row.get(2)?,
        owner: row.get(3)?,
        due: row.get::<_, Option<i64>>(4)?.map(EpochMs::new),
        done: row.get(5)?,
    })
}

/// The text indexed for full-text search: summary, decisions and open questions.
fn searchable_text(note: &Note) -> String {
    let mut text = note.summary.clone();
    for line in note.decisions.iter().chain(&note.open_questions) {
        text.push('\n');
        text.push_str(line);
    }
    text
}

/// SQLite-backed [`NoteRepository`].
#[derive(Debug, Clone)]
pub struct SqliteNoteRepository {
    db: Database,
}

impl SqliteNoteRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl NoteRepository for SqliteNoteRepository {
    async fn save(&self, note: &Note, action_items: &[ActionItem]) -> Result<(), RepositoryError> {
        if let Some(stray) = action_items.iter().find(|item| item.note_id != note.id) {
            return Err(RepositoryError::Invalid(format!(
                "action item {} belongs to note {}, not {}",
                stray.id, stray.note_id, note.id
            )));
        }
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            let note_id = note.id.as_str();

            tx.execute(
                "INSERT INTO notes (id, session_id, language, summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET
                     session_id = excluded.session_id,
                     language = excluded.language,
                     summary = excluded.summary,
                     created_at = excluded.created_at",
                params![
                    note_id,
                    note.session_id.as_str(),
                    note.language.as_str(),
                    note.summary,
                    note.created_at.get(),
                ],
            )?;

            tx.execute(
                "DELETE FROM note_items WHERE note_id = ?1",
                params![note_id],
            )?;
            for (kind, texts) in [
                (KIND_DECISION, &note.decisions),
                (KIND_OPEN_QUESTION, &note.open_questions),
            ] {
                for (position, text) in (0_i64..).zip(texts.iter()) {
                    tx.execute(
                        "INSERT INTO note_items (note_id, kind, position, text)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![note_id, kind, position, text],
                    )?;
                }
            }

            tx.execute(
                "DELETE FROM note_frame_refs WHERE note_id = ?1",
                params![note_id],
            )?;
            for (position, frame_id) in (0_i64..).zip(note.frame_refs.iter()) {
                tx.execute(
                    "INSERT INTO note_frame_refs (note_id, position, frame_id)
                     VALUES (?1, ?2, ?3)",
                    params![note_id, position, frame_id.as_str()],
                )?;
            }

            tx.execute(
                "DELETE FROM action_items WHERE note_id = ?1",
                params![note_id],
            )?;
            for (position, item) in (0_i64..).zip(action_items.iter()) {
                tx.execute(
                    "INSERT INTO action_items (id, note_id, position, task, owner, due, done)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        item.id.as_str(),
                        note_id,
                        position,
                        item.task,
                        item.owner,
                        item.due.map(EpochMs::get),
                        item.done,
                    ],
                )?;
            }

            remove_ref_docs(&tx, SOURCE_NOTE, note_id)?;
            add_doc(
                &tx,
                &note.session_id,
                SOURCE_NOTE,
                note_id,
                None,
                &searchable_text(note),
            )?;

            tx.commit()?;
            Ok(())
        })
    }

    async fn get(&self, id: &NoteId) -> Result<Option<Note>, RepositoryError> {
        self.db.with_conn(|conn| {
            load_note(
                conn,
                &format!("SELECT {NOTE_COLUMNS} FROM notes WHERE id = ?1"),
                params![id.as_str()],
            )
        })
    }

    async fn latest_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Note>, RepositoryError> {
        self.db.with_conn(|conn| {
            load_note(
                conn,
                &format!(
                    "SELECT {NOTE_COLUMNS} FROM notes WHERE session_id = ?1
                     ORDER BY created_at DESC, id DESC LIMIT 1"
                ),
                params![session_id.as_str()],
            )
        })
    }

    async fn delete(&self, id: &NoteId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            remove_ref_docs(&tx, SOURCE_NOTE, id.as_str())?;
            let changed = tx.execute("DELETE FROM notes WHERE id = ?1", params![id.as_str()])?;
            require_changed(changed, "note", id.as_str())?;
            tx.commit()?;
            Ok(())
        })
    }

    async fn list_action_items(
        &self,
        note_id: &NoteId,
    ) -> Result<Vec<ActionItem>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, note_id, task, owner, due, done
                 FROM action_items WHERE note_id = ?1 ORDER BY position",
            )?;
            let rows = stmt.query_map(params![note_id.as_str()], action_item_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn upsert_action_item(&self, item: &ActionItem) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO action_items (id, note_id, position, task, owner, due, done)
                 VALUES (?1, ?2,
                         (SELECT COALESCE(MAX(position) + 1, 0) FROM action_items
                          WHERE note_id = ?2),
                         ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     note_id = excluded.note_id,
                     task = excluded.task,
                     owner = excluded.owner,
                     due = excluded.due,
                     done = excluded.done",
                params![
                    item.id.as_str(),
                    item.note_id.as_str(),
                    item.task,
                    item.owner,
                    item.due.map(EpochMs::get),
                    item.done,
                ],
            )?;
            Ok(())
        })
    }

    async fn set_action_item_done(
        &self,
        id: &ActionItemId,
        done: bool,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE action_items SET done = ?2 WHERE id = ?1",
                params![id.as_str(), done],
            )?;
            require_changed(changed, "action item", id.as_str())
        })
    }

    async fn delete_action_item(&self, id: &ActionItemId) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "DELETE FROM action_items WHERE id = ?1",
                params![id.as_str()],
            )?;
            require_changed(changed, "action item", id.as_str())
        })
    }
}
