//! SQLite connection handling and schema migrations.
//!
//! Library choice: `rusqlite` with the bundled SQLite (FTS5 compiled in) rather than `sqlx`.
//! The app is a single-user local desktop program, so a connection pool and compile-time query
//! checking buy little, while `sqlx` would pull in a runtime and a much larger dependency tree
//! next to Tauri. The application ports are async-shaped, but the calls here are short local
//! SQLite statements executed synchronously.
//!
//! Migrations are plain SQL files embedded at compile time and tracked with `PRAGMA
//! user_version`. Each migration runs in its own transaction, so a failed one leaves the
//! database at the previous version.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, ErrorCode, TransactionBehavior};
use tan_application::RepositoryError;

/// Failure to open or migrate the database.
#[derive(Debug)]
pub enum StorageError {
    /// SQLite reported an error.
    Sqlite(rusqlite::Error),
    /// Creating the data directory failed.
    Io(std::io::Error),
    /// The database was written by a newer version of the application.
    SchemaTooNew {
        /// Version found in the file.
        found: i64,
        /// Newest version this build understands.
        supported: i64,
    },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::SchemaTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than the supported version {supported}"
            ),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::SchemaTooNew { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

struct Migration {
    version: i64,
    up: &'static str,
    down: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        up: include_str!("../migrations/0001_init.sql"),
        down: include_str!("../migrations/0001_init.down.sql"),
    },
    Migration {
        version: 2,
        up: include_str!("../migrations/0002_provider_vision.sql"),
        down: include_str!("../migrations/0002_provider_vision.down.sql"),
    },
    Migration {
        version: 3,
        up: include_str!("../migrations/0003_processing.sql"),
        down: include_str!("../migrations/0003_processing.down.sql"),
    },
];

/// Newest schema version this build knows; derived from the migration list so the two cannot
/// drift apart.
pub const LATEST_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// The schema version recorded in the database (`0` for a brand-new file).
pub fn schema_version(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Applies every pending migration. Running it on an up-to-date database is a no-op.
pub fn migrate_up(conn: &mut Connection) -> Result<(), StorageError> {
    for migration in MIGRATIONS {
        // IMMEDIATE takes the write lock first, so two processes opening the same new file
        // cannot both decide the migration is pending.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = schema_version(&tx)?;
        if current > LATEST_SCHEMA_VERSION {
            return Err(StorageError::SchemaTooNew {
                found: current,
                supported: LATEST_SCHEMA_VERSION,
            });
        }
        if current >= migration.version {
            continue;
        }
        tx.execute_batch(migration.up)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(())
}

/// Reverts migrations until the schema is at `target` (use `0` for an empty database).
///
/// Destructive; intended for development and tests.
pub fn migrate_down_to(conn: &mut Connection, target: i64) -> Result<(), StorageError> {
    for migration in MIGRATIONS.iter().rev() {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = schema_version(&tx)?;
        if current < migration.version || migration.version <= target {
            continue;
        }
        tx.execute_batch(migration.down)?;
        tx.pragma_update(None, "user_version", migration.version - 1)?;
        tx.commit()?;
    }
    Ok(())
}

fn configure(conn: &Connection, file_backed: bool) -> Result<(), StorageError> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    if file_backed {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

/// Shared handle to the application database. Cheap to clone.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    /// Opens (creating if needed) the database file and migrates it to the latest schema.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        configure(&conn, true)?;
        migrate_up(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Opens a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn, false)?;
        migrate_up(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in a bad state (statements are
        // atomic), so a poisoned mutex is safe to keep using.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` with the connection, translating failures into [`RepositoryError`].
    pub(crate) fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, DbError>,
    ) -> Result<T, RepositoryError> {
        let mut guard = self.lock();
        f(&mut guard).map_err(DbError::into_repository_error)
    }
}

/// Internal error that carries either a raw SQLite error or an already-classified one.
#[derive(Debug)]
pub(crate) enum DbError {
    Sqlite(rusqlite::Error),
    Repository(RepositoryError),
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<RepositoryError> for DbError {
    fn from(e: RepositoryError) -> Self {
        Self::Repository(e)
    }
}

impl DbError {
    fn into_repository_error(self) -> RepositoryError {
        use rusqlite::Error as E;
        match self {
            Self::Repository(e) => e,
            Self::Sqlite(E::SqliteFailure(failure, message))
                if failure.code == ErrorCode::ConstraintViolation =>
            {
                RepositoryError::Conflict(
                    message.unwrap_or_else(|| "constraint violation".to_owned()),
                )
            }
            Self::Sqlite(
                e @ (E::FromSqlConversionFailure(..)
                | E::InvalidColumnType(..)
                | E::IntegralValueOutOfRange(..)),
            ) => RepositoryError::CorruptData(e.to_string()),
            Self::Sqlite(e @ E::ToSqlConversionFailure(_)) => {
                RepositoryError::Invalid(e.to_string())
            }
            Self::Sqlite(e) => RepositoryError::storage(e),
        }
    }
}

/// Converts a `u64` (milliseconds, byte counts) into SQLite's signed integer.
pub(crate) fn u64_to_sql(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// Reads an INTEGER column as `u64`; negative values are reported as corrupt data.
pub(crate) fn u64_from_row(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(idx)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(idx, value))
}

#[cfg(test)]
mod tests {
    use super::{
        migrate_down_to, migrate_up, schema_version, Database, LATEST_SCHEMA_VERSION, MIGRATIONS,
    };
    use rusqlite::Connection;

    #[test]
    fn migration_versions_start_at_one_and_are_consecutive() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let expected = i64::try_from(index).unwrap() + 1;
            assert_eq!(migration.version, expected, "migration #{index}");
            assert!(
                !migration.up.trim().is_empty(),
                "version {expected} has no up script"
            );
            assert!(
                !migration.down.trim().is_empty(),
                "version {expected} has no down script"
            );
        }
        assert_eq!(
            LATEST_SCHEMA_VERSION,
            i64::try_from(MIGRATIONS.len()).unwrap()
        );
    }

    #[test]
    fn migrations_are_idempotent_and_reversible() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 0);
        migrate_up(&mut conn).unwrap();
        migrate_up(&mut conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
        migrate_down_to(&mut conn, 0).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 0);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "down migration must remove every object");
        migrate_up(&mut conn).unwrap();
    }

    #[test]
    fn newer_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            migrate_up(&mut conn),
            Err(super::StorageError::SchemaTooNew { .. })
        ));
    }

    #[test]
    fn open_in_memory_enables_foreign_keys() {
        let db = Database::open_in_memory().unwrap();
        let fk: i64 = db
            .with_conn(|c| Ok(c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(fk, 1);
    }
}
