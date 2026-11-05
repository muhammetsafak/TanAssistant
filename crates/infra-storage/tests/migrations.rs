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
