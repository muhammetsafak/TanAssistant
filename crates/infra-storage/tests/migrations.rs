// `assert!(x.is_empty())` / `assert!(a == b)` read fine in tests; pedantic clippy would rewrite them.
#![allow(clippy::assert_is_empty, clippy::manual_assert_eq)]

mod common;

use common::{block_on, session, test_db};
use rusqlite::Connection;
use tan_application::ports::SessionRepository;
use tan_domain::SessionId;
use tan_infra_storage::db::{migrate_down_to, migrate_up, schema_version};
use tan_infra_storage::{Database, SqliteSessionRepository, LATEST_SCHEMA_VERSION};

#[test]
fn migrations_build_an_empty_database_and_rerun_without_error() {
    let mut conn = Connection::open_in_memory().unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 0);

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);

    // Second run on the same database must be a no-op, not an error.
    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);

    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        tables >= 12,
        "expected the full schema, found {tables} tables"
    );
}

#[test]
fn reopening_a_file_database_keeps_data_and_does_not_remigrate() {
    let t = test_db();
    let repo = SqliteSessionRepository::new(t.db.clone());
    block_on(repo.create(&session("s1", 1_000))).unwrap();
    drop(repo);

    let reopened = Database::open(&t.path).expect("second open must succeed");
    let repo = SqliteSessionRepository::new(reopened);
    let loaded = block_on(repo.get(&SessionId::new("s1"))).unwrap();
    assert_eq!(loaded.map(|s| s.title), Some("Meeting s1".to_owned()));
}

#[test]
fn down_migration_empties_the_schema_and_up_restores_it() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up(&mut conn).unwrap();
    migrate_down_to(&mut conn, 0).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 0);
    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap();
    stmt.query_map([table], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn secrets_table_has_no_value_column() {
    let t = test_db();
    let conn = Connection::open(&t.path).unwrap();

    assert_eq!(
        columns(&conn, "secrets"),
        ["id", "name", "description", "created_at"]
    );

    // Defence in depth: nothing in the schema that could hold a secret value.
    let forbidden = [
        "value",
        "secret",
        "token",
        "password",
        "api_key",
        "key_material",
    ];
    for table in ["secrets", "provider_profiles"] {
        for column in columns(&conn, table) {
            assert!(
                !forbidden.contains(&column.as_str()),
                "{table}.{column} looks like a secret value column"
            );
        }
    }
}

#[test]
fn fts_table_uses_the_diacritic_preserving_tokenizer() {
    let t = test_db();
    let conn = Connection::open(&t.path).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'search_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(sql.contains("fts5"), "{sql}");
    assert!(sql.contains("unicode61 remove_diacritics 0"), "{sql}");
}

/// A database at schema version 1 with profiles already in it, as an installed app would have.
fn version_one_database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
        .unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    for (id, kind) in [("p-openai", "openai"), ("p-ollama", "ollama")] {
        conn.execute(
            "INSERT INTO provider_profiles (id, kind, display_name, model) VALUES (?1, ?2, ?2, 'm')",
            [id, kind],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO task_assignments (task, profile_id) VALUES ('summary', 'p-openai')",
        [],
    )
    .unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 1);
    conn
}

fn vision_flags(conn: &Connection) -> Vec<(String, i64)> {
    let mut stmt = conn
        .prepare("SELECT id, supports_vision FROM provider_profiles ORDER BY id")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn vision_migration_keeps_existing_data_and_backfills_the_flag() {
    let mut conn = version_one_database();
    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);

    // Ollama defaults to "no vision"; the matrix providers are backfilled.
    assert_eq!(
        vision_flags(&conn),
        [("p-ollama".to_owned(), 0), ("p-openai".to_owned(), 1)]
    );
    let assignments: i64 = conn
        .query_row("SELECT count(*) FROM task_assignments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(assignments, 1, "existing rows are untouched");

    // The flag is a strict boolean.
    assert!(conn
        .execute("UPDATE provider_profiles SET supports_vision = 2", [])
        .is_err());
}

#[test]
fn vision_migration_reverts_to_version_one_without_losing_profiles() {
    let mut conn = version_one_database();
    migrate_up(&mut conn).unwrap();
    migrate_down_to(&mut conn, 1).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 1);
    assert_eq!(
        columns(&conn, "provider_profiles"),
        [
            "id",
            "kind",
            "display_name",
            "model",
            "base_url",
            "secret_ref"
        ]
    );
    let profiles: i64 = conn
        .query_row("SELECT count(*) FROM provider_profiles", [], |r| r.get(0))
        .unwrap();
    assert_eq!(profiles, 2);
    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
}

#[test]
fn a_fresh_database_has_the_vision_column_defaulting_to_false() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up(&mut conn).unwrap();
    assert!(columns(&conn, "provider_profiles").contains(&"supports_vision".to_owned()));
    conn.execute(
        "INSERT INTO provider_profiles (id, kind, display_name, model) VALUES ('x', 'ollama', 'x', 'm')",
        [],
    )
    .unwrap();
    assert_eq!(vision_flags(&conn), [("x".to_owned(), 0)]);
}

/// A database at schema version 4 with a session in it, as an installed app would have.
fn version_four_database() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up(&mut conn).unwrap();
    migrate_down_to(&mut conn, 4).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 4);
    conn.execute(
        "INSERT INTO sessions (id, kind, title, started_at, language, note_language, status)
         VALUES ('s1', 'meeting', 'Old meeting', 1000, 'auto', 'tr', 'recorded')",
        [],
    )
    .unwrap();
    conn
}

#[test]
fn track_mutes_migration_keeps_existing_data_and_adds_the_table() {
    let mut conn = version_four_database();
    assert!(columns(&conn, "track_mutes").is_empty());

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    assert_eq!(
        columns(&conn, "track_mutes"),
        ["id", "session_id", "kind", "start_ms", "end_ms"]
    );
    let sessions: i64 = conn
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1, "existing rows are untouched");

    // An open interval has no end; an end before the start is rejected.
    conn.execute(
        "INSERT INTO track_mutes (session_id, kind, start_ms, end_ms) VALUES ('s1', 'microphone', 5, NULL)",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO track_mutes (session_id, kind, start_ms, end_ms) VALUES ('s1', 'system', 9, 3)",
            [],
        )
        .is_err());

    // The rows go with their session.
    conn.execute("DELETE FROM sessions WHERE id = 's1'", [])
        .unwrap();
    let mutes: i64 = conn
        .query_row("SELECT count(*) FROM track_mutes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mutes, 0);
}

#[test]
fn track_mutes_migration_reverts_to_version_four_without_touching_sessions() {
    let mut conn = version_four_database();
    migrate_up(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO track_mutes (session_id, kind, start_ms, end_ms) VALUES ('s1', 'microphone', 5, 8)",
        [],
    )
    .unwrap();

    migrate_down_to(&mut conn, 4).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 4);
    assert!(columns(&conn, "track_mutes").is_empty());
    let sessions: i64 = conn
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
}

/// A database at schema version 5 with a session, a note and two action items (one owned by the
/// old free-text column), as an installed app would have.
fn version_five_database() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up(&mut conn).unwrap();
    migrate_down_to(&mut conn, 5).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 5);
    conn.execute_batch(
        "INSERT INTO sessions (id, kind, title, started_at, language, note_language, status)
         VALUES ('s1', 'meeting', 'Old meeting', 1000, 'auto', 'tr', 'processed');
         INSERT INTO notes (id, session_id, language, summary, created_at)
         VALUES ('n1', 's1', 'tr', 'Summary', 1000);
         INSERT INTO action_items (id, note_id, position, task, owner, due, done)
         VALUES ('a1', 'n1', 0, 'Raporu hazırla', 'Özge', 5, 1),
                ('a2', 'n1', 1, 'Sunumu yap', NULL, NULL, 0);",
    )
    .unwrap();
    conn
}

#[test]
fn people_migration_adds_the_schema_and_keeps_existing_rows() {
    let mut conn = version_five_database();
    assert!(columns(&conn, "people").is_empty());

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    assert_eq!(
        columns(&conn, "people"),
        [
            "id",
            "display_name",
            "normalized_name",
            "is_self",
            "created_at"
        ]
    );
    assert_eq!(
        columns(&conn, "session_participants"),
        ["session_id", "person_id", "source"]
    );
    let item_columns = columns(&conn, "action_items");
    assert!(item_columns.contains(&"owner_person_id".to_owned()));
    assert!(item_columns.contains(&"owner_edited".to_owned()));

    // Existing items are untouched: the old text stays where it was, unlinked and not edited.
    let (owner, person, edited, done): (Option<String>, Option<String>, i64, i64) = conn
        .query_row(
            "SELECT owner, owner_person_id, owner_edited, done FROM action_items WHERE id = 'a1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (owner.as_deref(), person, edited, done),
        (Some("Özge"), None, 0, 1)
    );

    // Exactly one "me" person is seeded and there cannot be a second one.
    let people: Vec<(String, i64)> = conn
        .prepare("SELECT id, is_self FROM people")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(people, [("self".to_owned(), 1)]);
    assert!(conn
        .execute(
            "INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
             VALUES ('p9', 'Other me', 'other me', 1, 0)",
            [],
        )
        .is_err());
}

#[test]
fn people_are_unique_by_normalized_name_and_the_references_cascade() {
    let mut conn = version_five_database();
    migrate_up(&mut conn).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute(
        "INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
         VALUES ('p1', 'Özge', 'özge', 0, 0)",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
             VALUES ('p2', 'ÖZGE', 'özge', 0, 0)",
            [],
        )
        .is_err());
    conn.execute(
        "UPDATE action_items SET owner_person_id = 'p1', owner_edited = 1 WHERE id = 'a1'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_participants (session_id, person_id, source) VALUES ('s1', 'p1', 'ai')",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_participants (session_id, person_id, source) VALUES ('s1', 'p1', 'robot')",
            [],
        )
        .is_err(),
        "the source is one of ai/manual (and the pair is unique)"
    );
    assert!(conn
        .execute(
            "UPDATE action_items SET owner_person_id = 'ghost' WHERE id = 'a2'",
            []
        )
        .is_err());

    conn.execute("DELETE FROM people WHERE id = 'p1'", [])
        .unwrap();
    let owner: Option<String> = conn
        .query_row(
            "SELECT owner_person_id FROM action_items WHERE id = 'a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, None, "the item stays, ownerless");
    let participants: i64 = conn
        .query_row("SELECT count(*) FROM session_participants", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(participants, 0);
}

#[test]
fn people_migration_reverts_to_version_five_and_keeps_the_owners_as_text() {
    let mut conn = version_five_database();
    migrate_up(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO people (id, display_name, normalized_name, is_self, created_at)
         VALUES ('p1', 'İlker', 'ilker', 0, 0)",
        [],
    )
    .unwrap();
    // a1 is linked to a person (the old text is also still there), a2 to the "me" person.
    conn.execute(
        "UPDATE action_items SET owner_person_id = 'p1', owner_edited = 1 WHERE id = 'a1'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE action_items SET owner_person_id = 'self' WHERE id = 'a2'",
        [],
    )
    .unwrap();

    migrate_down_to(&mut conn, 5).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 5);
    assert!(columns(&conn, "people").is_empty());
    assert!(columns(&conn, "session_participants").is_empty());
    assert_eq!(
        columns(&conn, "action_items"),
        ["id", "note_id", "position", "task", "owner", "due", "done"]
    );
    let items: Vec<(String, Option<String>, i64, i64)> = conn
        .prepare("SELECT id, owner, due IS NOT NULL, done FROM action_items ORDER BY position")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        items,
        [
            ("a1".to_owned(), Some("İlker".to_owned()), 1, 1),
            ("a2".to_owned(), Some("Me".to_owned()), 0, 0),
        ],
        "the owners survive as text and nothing else about the items changes"
    );
    // The rebuilt table still hangs off its note.
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute("DELETE FROM notes WHERE id = 'n1'", [])
        .unwrap();
    let left: i64 = conn
        .query_row("SELECT count(*) FROM action_items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
}

/// A database at schema version 6 with a session, a note and an action item, as an installed
/// app would have.
fn version_six_database() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up(&mut conn).unwrap();
    migrate_down_to(&mut conn, 6).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 6);
    conn.execute_batch(
        "INSERT INTO sessions (id, kind, title, started_at, language, note_language, status)
         VALUES ('s1', 'meeting', 'Old meeting', 1000, 'auto', 'tr', 'processed');
         INSERT INTO notes (id, session_id, language, summary, created_at)
         VALUES ('n1', 's1', 'tr', 'Summary', 1000);
         INSERT INTO action_items (id, note_id, position, task, due, done)
         VALUES ('a1', 'n1', 0, 'Raporu hazırla', 5, 1);",
    )
    .unwrap();
    conn
}

#[test]
fn action_briefs_migration_adds_the_table_and_keeps_existing_rows() {
    let mut conn = version_six_database();
    assert!(columns(&conn, "action_briefs").is_empty());

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    assert_eq!(
        columns(&conn, "action_briefs"),
        [
            "action_item_id",
            "language",
            "content_json",
            "provider_profile_id",
            "created_at"
        ]
    );
    let items: i64 = conn
        .query_row("SELECT count(*) FROM action_items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(items, 1, "existing rows are untouched");
}

#[test]
fn action_briefs_are_one_per_item_and_cascade_with_it() {
    let mut conn = version_six_database();
    migrate_up(&mut conn).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute(
        "INSERT INTO action_briefs (action_item_id, language, content_json, created_at)
         VALUES ('a1', 'tr', '{}', 1)",
        [],
    )
    .unwrap();
    // The item is the primary key; an unknown item is rejected.
    assert!(conn
        .execute(
            "INSERT INTO action_briefs (action_item_id, language, content_json, created_at)
             VALUES ('a1', 'tr', '{}', 2)",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO action_briefs (action_item_id, language, content_json, created_at)
             VALUES ('ghost', 'tr', '{}', 2)",
            [],
        )
        .is_err());

    conn.execute("DELETE FROM notes WHERE id = 'n1'", [])
        .unwrap();
    let briefs: i64 = conn
        .query_row("SELECT count(*) FROM action_briefs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(briefs, 0, "the brief goes with its note's action items");
}

#[test]
fn action_briefs_migration_reverts_to_version_six_without_touching_notes() {
    let mut conn = version_six_database();
    migrate_up(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO action_briefs (action_item_id, language, content_json, created_at)
         VALUES ('a1', 'tr', '{}', 1)",
        [],
    )
    .unwrap();

    migrate_down_to(&mut conn, 6).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 6);
    assert!(columns(&conn, "action_briefs").is_empty());
    let items: i64 = conn
        .query_row("SELECT count(*) FROM action_items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(items, 1);

    migrate_up(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
}
