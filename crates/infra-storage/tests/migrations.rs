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
