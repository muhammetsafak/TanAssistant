//! `PersonRepository` over SQLite, and the start-up move of old owner texts to people.

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension, Row, Transaction};
use tan_application::people::{clean_display_name, is_self_marker, normalize_name};
use tan_application::ports::PersonRepository;
use tan_application::RepositoryError;
use tan_domain::{EpochMs, ParticipantSource, Person, PersonId, SessionId, SessionParticipant};

use super::{parse_column, require_changed};
use crate::db::{Database, DbError};

const PERSON_COLUMNS: &str = "id, display_name, is_self, created_at";

fn person_from_row(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: PersonId::new(row.get::<_, String>(0)?),
        display_name: row.get(1)?,
        is_self: row.get(2)?,
        created_at: EpochMs::new(row.get(3)?),
    })
}

/// Replaces the AI-extracted participants of a session with `person_ids` inside `tx`; the
/// user's own rows are kept and win over the AI's.
pub(super) fn replace_ai_participants(
    tx: &Transaction<'_>,
    session_id: &SessionId,
    person_ids: &[PersonId],
) -> Result<(), DbError> {
    tx.execute(
        "DELETE FROM session_participants WHERE session_id = ?1 AND source = 'ai'",
        params![session_id.as_str()],
    )?;
    for person_id in person_ids {
        tx.execute(
            "INSERT INTO session_participants (session_id, person_id, source)
             VALUES (?1, ?2, 'ai') ON CONFLICT (session_id, person_id) DO NOTHING",
            params![session_id.as_str(), person_id.as_str()],
        )?;
    }
    Ok(())
}

/// SQLite-backed [`PersonRepository`].
#[derive(Debug, Clone)]
pub struct SqlitePersonRepository {
    db: Database,
}

impl SqlitePersonRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Moves the free-text owners that action items had before the person directory existed
    /// (migration 0006 only creates the schema) to people, and makes each of them a participant
    /// of the session the item belongs to. Returns how many action items were linked.
    ///
    /// It uses the application's name normalizer: SQLite's `lower()` only folds ASCII letters,
    /// so doing this in SQL would store "Özge" and "ÖZGE" as two people. It is idempotent: it
    /// only touches items that have an old owner text, no person and no choice by the user, so
    /// a second run finds nothing, and a name that is already a person is reused, never
    /// created twice.
    ///
    /// # Errors
    ///
    /// Fails when the database does; nothing is changed then (one transaction).
    pub fn backfill_legacy_owners(&self, now: EpochMs) -> Result<usize, RepositoryError> {
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            let legacy: Vec<(String, String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT a.id, a.owner, n.session_id
                     FROM action_items a JOIN notes n ON n.id = a.note_id
                     WHERE a.owner IS NOT NULL AND a.owner_person_id IS NULL
                       AND a.owner_edited = 0
                     ORDER BY n.created_at, n.id, a.position",
                )?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };

            let mut resolved: HashMap<String, String> = HashMap::new();
            let mut linked = 0;
            for (item_id, owner, session_id) in legacy {
                let is_self = is_self_marker(&owner);
                let key = if is_self {
                    String::new() // the "me" person's normalized name
                } else {
                    normalize_name(&owner)
                };
                if key.is_empty() && !is_self {
                    continue; // blank text: there is no one to link
                }
                let person_id = if let Some(known) = resolved.get(&key) {
                    known.clone()
                } else {
                    let existing: Option<String> = tx
                        .query_row(
                            "SELECT id FROM people WHERE normalized_name = ?1",
                            params![key],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let id = match existing {
                        Some(id) => id,
                        // A name that cannot be a person (too long) stays as it was.
                        None => match clean_display_name(&owner) {
                            Some(display_name) => {
                                let id = crate::new_id();
                                tx.execute(
                                    "INSERT INTO people
                                         (id, display_name, normalized_name, is_self, created_at)
                                     VALUES (?1, ?2, ?3, 0, ?4)",
                                    params![id, display_name, key, now.get()],
                                )?;
                                id
                            }
                            _ => continue,
                        },
                    };
                    resolved.insert(key, id.clone());
                    id
                };
                tx.execute(
                    "UPDATE action_items SET owner_person_id = ?2 WHERE id = ?1",
                    params![item_id, person_id],
                )?;
                tx.execute(
                    "INSERT INTO session_participants (session_id, person_id, source)
                     VALUES (?1, ?2, 'ai') ON CONFLICT (session_id, person_id) DO NOTHING",
                    params![session_id, person_id],
                )?;
                linked += 1;
            }
            tx.commit()?;
            Ok(linked)
        })
    }
}

impl PersonRepository for SqlitePersonRepository {
    async fn list(&self) -> Result<Vec<Person>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PERSON_COLUMNS} FROM people ORDER BY is_self DESC, normalized_name, id"
            ))?;
            let rows = stmt.query_map([], person_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn get(&self, id: &PersonId) -> Result<Option<Person>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {PERSON_COLUMNS} FROM people WHERE id = ?1"),
                    params![id.as_str()],
                    person_from_row,
                )
                .optional()?)
        })
    }

    async fn self_person(&self) -> Result<Person, RepositoryError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {PERSON_COLUMNS} FROM people WHERE is_self = 1"),
                [],
                person_from_row,
            )
            .optional()?
            .ok_or_else(|| DbError::from(RepositoryError::not_found("person", "self")))
        })
    }

    async fn find_or_create(
        &self,
        normalized_name: &str,
        candidate: &Person,
    ) -> Result<Person, RepositoryError> {
        if candidate.is_self || normalized_name.is_empty() {
            return Err(RepositoryError::Invalid(
                "only the seeded person is the \"me\" person, and a name cannot be empty"
                    .to_owned(),
            ));
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4)
                 ON CONFLICT (normalized_name) DO NOTHING",
                params![
                    candidate.id.as_str(),
                    candidate.display_name,
                    normalized_name,
                    candidate.created_at.get()
                ],
            )?;
            Ok(conn.query_row(
                &format!("SELECT {PERSON_COLUMNS} FROM people WHERE normalized_name = ?1"),
                params![normalized_name],
                person_from_row,
            )?)
        })
    }

    async fn list_participants(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionParticipant>, RepositoryError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT person_id, source FROM session_participants
                 WHERE session_id = ?1 ORDER BY rowid",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |row| {
                Ok(SessionParticipant {
                    session_id: session_id.clone(),
                    person_id: PersonId::new(row.get::<_, String>(0)?),
                    source: parse_column::<ParticipantSource>(row, 1)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    async fn add_participant(
        &self,
        participant: &SessionParticipant,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO session_participants (session_id, person_id, source)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (session_id, person_id) DO UPDATE SET source = excluded.source
                 WHERE excluded.source = 'manual'",
                params![
                    participant.session_id.as_str(),
                    participant.person_id.as_str(),
                    participant.source.as_str()
                ],
            )?;
            Ok(())
        })
    }

    async fn remove_participant(
        &self,
        session_id: &SessionId,
        person_id: &PersonId,
    ) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "DELETE FROM session_participants WHERE session_id = ?1 AND person_id = ?2",
                params![session_id.as_str(), person_id.as_str()],
            )?;
            require_changed(changed, "participant", person_id.as_str())
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_executor::block_on;
    use tan_application::ports::{NoteRepository, SessionRepository};
    use tan_domain::{
        ActionItem, ActionItemId, LanguageCode, LanguageSetting, Note, NoteId, Session, SessionKind,
    };

    use super::*;
    use crate::repositories::{SqliteNoteRepository, SqliteSessionRepository};

    const NOW: EpochMs = EpochMs::new(1_000);

    fn person(id: &str, name: &str) -> Person {
        Person {
            id: PersonId::new(id),
            display_name: name.to_owned(),
            is_self: false,
            created_at: NOW,
        }
    }

    /// A database with sessions `s1` and `s2`, each holding a note with action items.
    fn legacy_database() -> Database {
        let db = Database::open_in_memory().unwrap();
        let sessions = SqliteSessionRepository::new(db.clone());
        let notes = SqliteNoteRepository::new(db.clone());
        for (number, id) in ["s1", "s2"].into_iter().enumerate() {
            block_on(sessions.create(&Session::start_recording(
                SessionId::new(id),
                SessionKind::Meeting,
                format!("Meeting {id}"),
                EpochMs::new(1_000 + i64::try_from(number).unwrap()),
                LanguageSetting::Auto,
                LanguageCode::turkish(),
            )))
            .unwrap();
            let note_id = format!("n-{id}");
            let items: Vec<ActionItem> = (0..3)
                .map(|index| ActionItem {
                    id: ActionItemId::new(format!("a-{id}-{index}")),
                    note_id: NoteId::new(note_id.clone()),
                    task: format!("Task {index}"),
                    owner_person_id: None,
                    owner_edited: false,
                    due: None,
                    done: false,
                })
                .collect();
            block_on(notes.save(
                &Note {
                    id: NoteId::new(note_id),
                    session_id: SessionId::new(id),
                    language: LanguageCode::turkish(),
                    summary: "Summary".to_owned(),
                    decisions: vec![],
                    open_questions: vec![],
                    frame_refs: vec![],
                    created_at: NOW,
                },
                &items,
            ))
            .unwrap();
        }
        db
    }

    fn set_legacy_owner(db: &Database, item_id: &str, owner: Option<&str>) {
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE action_items SET owner = ?2 WHERE id = ?1",
                params![item_id, owner],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn owners_by_item(db: &Database) -> Vec<(String, Option<String>)> {
        db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT a.id, p.display_name FROM action_items a
                 LEFT JOIN people p ON p.id = a.owner_person_id ORDER BY a.id",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap()
    }

    #[test]
    fn the_seeded_self_person_exists_and_leads_the_list() {
        let repo = SqlitePersonRepository::new(Database::open_in_memory().unwrap());
        let me = block_on(repo.self_person()).unwrap();
        assert!(me.is_self);
        let people = block_on(repo.list()).unwrap();
        assert_eq!(people, [me]);
    }

    #[test]
    fn find_or_create_is_keyed_by_the_normalized_name() {
        let repo = SqlitePersonRepository::new(Database::open_in_memory().unwrap());
        let first = block_on(repo.find_or_create("ayşe", &person("p1", "Ayşe"))).unwrap();
        let again = block_on(repo.find_or_create("ayşe", &person("p2", "AYŞE"))).unwrap();
        assert_eq!(first.id, PersonId::new("p1"));
        assert_eq!(again, first, "the first spelling stays");
        let ascii = block_on(repo.find_or_create("ayse", &person("p3", "Ayse"))).unwrap();
        assert_ne!(ascii.id, first.id, "accents are kept: another person");
        assert_eq!(block_on(repo.list()).unwrap().len(), 3);

        let mut impostor = person("p9", "Me too");
        impostor.is_self = true;
        assert!(block_on(repo.find_or_create("me too", &impostor)).is_err());
        assert!(block_on(repo.find_or_create("", &person("p8", " "))).is_err());
    }

    #[test]
    fn participants_keep_the_users_choice_over_the_ais() {
        let db = legacy_database();
        let repo = SqlitePersonRepository::new(db.clone());
        let s1 = SessionId::new("s1");
        for (id, name) in [("p1", "Ayşe"), ("p2", "Mehmet"), ("p3", "Can")] {
            block_on(repo.find_or_create(&normalize_name(name), &person(id, name))).unwrap();
        }
        let add = |person: &str, source| SessionParticipant {
            session_id: s1.clone(),
            person_id: PersonId::new(person),
            source,
        };
        block_on(repo.add_participant(&add("p1", ParticipantSource::Ai))).unwrap();
        block_on(repo.add_participant(&add("p2", ParticipantSource::Manual))).unwrap();
        // The AI never downgrades the user's row; the user upgrades the AI's.
        block_on(repo.add_participant(&add("p2", ParticipantSource::Ai))).unwrap();
        block_on(repo.add_participant(&add("p1", ParticipantSource::Manual))).unwrap();
        let listed = block_on(repo.list_participants(&s1)).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|p| (p.person_id.as_str(), p.source))
                .collect::<Vec<_>>(),
            [
                ("p1", ParticipantSource::Manual),
                ("p2", ParticipantSource::Manual)
            ]
        );

        // Regenerating the note replaces only what the AI added.
        block_on(repo.add_participant(&add("p3", ParticipantSource::Ai))).unwrap();
        let notes = SqliteNoteRepository::new(db);
        block_on(notes.replace_for_session(
            &Note {
                id: NoteId::new("n-new"),
                session_id: s1.clone(),
                language: LanguageCode::turkish(),
                summary: "Summary".to_owned(),
                decisions: vec![],
                open_questions: vec![],
                frame_refs: vec![],
                created_at: NOW,
            },
            &[],
            &[PersonId::new("p2"), PersonId::new("p1")],
        ))
        .unwrap();
        let after = block_on(repo.list_participants(&s1)).unwrap();
        let ids: Vec<&str> = after.iter().map(|p| p.person_id.as_str()).collect();
        assert_eq!(
            ids,
            ["p1", "p2"],
            "the AI's stale p3 is gone, the user's rows stay"
        );
        assert!(after.iter().all(|p| p.source == ParticipantSource::Manual));

        block_on(repo.remove_participant(&s1, &PersonId::new("p1"))).unwrap();
        assert!(matches!(
            block_on(repo.remove_participant(&s1, &PersonId::new("p1"))),
            Err(RepositoryError::NotFound { .. })
        ));
        assert!(matches!(
            block_on(repo.add_participant(&SessionParticipant {
                session_id: SessionId::new("ghost"),
                person_id: PersonId::new("p1"),
                source: ParticipantSource::Manual,
            })),
            Err(RepositoryError::Conflict(_))
        ));
    }

    #[test]
    fn backfill_turns_old_owner_texts_into_people_and_participants_once() {
        let db = legacy_database();
        set_legacy_owner(&db, "a-s1-0", Some("Özge"));
        set_legacy_owner(&db, "a-s1-1", Some("ÖZGE"));
        set_legacy_owner(&db, "a-s2-0", Some("İlker"));
        set_legacy_owner(&db, "a-s2-1", Some("  ilker "));
        set_legacy_owner(&db, "a-s2-2", Some("me"));
        set_legacy_owner(&db, "a-s1-2", Some("   "));
        let repo = SqlitePersonRepository::new(db.clone());

        assert_eq!(repo.backfill_legacy_owners(NOW).unwrap(), 5);
        let people = block_on(repo.list()).unwrap();
        assert_eq!(
            people
                .iter()
                .map(|p| p.display_name.as_str())
                .collect::<Vec<_>>(),
            ["Me", "İlker", "Özge"],
            "one person per name, the first spelling is kept"
        );
        assert_eq!(
            owners_by_item(&db),
            [
                ("a-s1-0".to_owned(), Some("Özge".to_owned())),
                ("a-s1-1".to_owned(), Some("Özge".to_owned())),
                ("a-s1-2".to_owned(), None),
                ("a-s2-0".to_owned(), Some("İlker".to_owned())),
                ("a-s2-1".to_owned(), Some("İlker".to_owned())),
                ("a-s2-2".to_owned(), Some("Me".to_owned())),
            ]
        );
        let participants = |session: &str| -> Vec<String> {
            let listed = block_on(repo.list_participants(&SessionId::new(session))).unwrap();
            let mut names: Vec<String> = listed
                .iter()
                .map(|p| {
                    people
                        .iter()
                        .find(|person| person.id == p.person_id)
                        .unwrap()
                        .display_name
                        .clone()
                })
                .collect();
            assert!(listed.iter().all(|p| p.source == ParticipantSource::Ai));
            names.sort();
            names
        };
        assert_eq!(participants("s1"), ["Özge"]);
        assert_eq!(participants("s2"), ["Me", "İlker"]);

        // A second run changes nothing, and a later lookup of the same name finds the person.
        assert_eq!(repo.backfill_legacy_owners(NOW).unwrap(), 0);
        assert_eq!(block_on(repo.list()).unwrap(), people);
        let again =
            block_on(repo.find_or_create(&normalize_name("ÖZGE"), &person("new", "ÖZGE"))).unwrap();
        assert_eq!(again.display_name, "Özge");
        assert_eq!(block_on(repo.list()).unwrap().len(), people.len());
    }

    #[test]
    fn backfill_leaves_a_cleared_or_chosen_owner_alone() {
        let db = legacy_database();
        set_legacy_owner(&db, "a-s1-0", Some("Özge"));
        set_legacy_owner(&db, "a-s1-1", Some("Özge"));
        let notes = SqliteNoteRepository::new(db.clone());
        // The user cleared the owner of the first item before the start-up move ran.
        block_on(notes.set_action_item_owner(&ActionItemId::new("a-s1-0"), None)).unwrap();
        let repo = SqlitePersonRepository::new(db.clone());
        assert_eq!(repo.backfill_legacy_owners(NOW).unwrap(), 1);
        assert_eq!(
            owners_by_item(&db)[..2],
            [
                ("a-s1-0".to_owned(), None),
                ("a-s1-1".to_owned(), Some("Özge".to_owned()))
            ]
        );
    }

    #[test]
    fn choosing_an_owner_marks_it_edited_and_an_unknown_person_is_refused() {
        let db = legacy_database();
        let repo = SqlitePersonRepository::new(db.clone());
        let notes = SqliteNoteRepository::new(db);
        block_on(repo.find_or_create("ayşe", &person("p1", "Ayşe"))).unwrap();
        let item = ActionItemId::new("a-s1-0");
        block_on(notes.set_action_item_owner(&item, Some(&PersonId::new("p1")))).unwrap();
        let items = block_on(notes.list_action_items(&NoteId::new("n-s1"))).unwrap();
        assert_eq!(items[0].owner_person_id, Some(PersonId::new("p1")));
        assert!(items[0].owner_edited);
        assert!(!items[1].owner_edited);

        assert!(matches!(
            block_on(notes.set_action_item_owner(&item, Some(&PersonId::new("ghost")))),
            Err(RepositoryError::Conflict(_))
        ));
        assert!(matches!(
            block_on(notes.set_action_item_owner(&ActionItemId::new("nope"), None)),
            Err(RepositoryError::NotFound { .. })
        ));
    }
}
