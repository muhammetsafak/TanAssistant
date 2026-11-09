//! Diagnostic logging: every `tracing` event goes to a daily file under `<data>/logs/`.
//!
//! Files are named `tan.YYYY-MM-DD.log` (UTC date, one per day) and each line carries a UTC
//! RFC 3339 timestamp. Writes are unbuffered so the lines just before a crash reach the disk;
//! panics are logged with their location and a backtrace. Files older than [`LOG_MAX_AGE`]
//! are deleted at startup and once a day after that.
//!
//! The level is `info` for Tan's crates and `warn` for dependencies; `TAN_LOG` overrides it
//! with a target list such as `debug` or `warn,tan_infra_ai=debug`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// Name of the log folder inside the data directory.
pub const LOGS_DIR: &str = "logs";
/// How long a log file is kept after its last write.
pub const LOG_MAX_AGE: Duration = Duration::from_hours(45 * 24);
/// Environment variable that overrides the default log filter.
pub const LOG_FILTER_ENV: &str = "TAN_LOG";

const FILE_PREFIX: &str = "tan";
const FILE_SUFFIX: &str = "log";
const DEFAULT_FILTER: &str = "warn,tan=info";
const PRUNE_INTERVAL: Duration = Duration::from_hours(24);

/// The log folder under the data directory `data_root`.
#[must_use]
pub fn logs_dir(data_root: &Path) -> PathBuf {
    data_root.join(LOGS_DIR)
}

/// Installs the global subscriber (daily file under `logs_dir`, plus stderr in debug builds),
/// the panic hook, and the background thread that deletes old log files.
///
/// Logging must never stop the app: on any failure the reason goes to stderr and the app runs
/// on (without a file log when the folder cannot be used).
pub fn init(logs_dir: &Path) {
    let filter = std::env::var(LOG_FILTER_ENV)
        .ok()
        .and_then(|value| value.parse::<Targets>().ok())
        .unwrap_or_else(|| {
            DEFAULT_FILTER
                .parse()
                .expect("the default log filter is valid")
        });

    let file_layer = match open_appender(logs_dir) {
        Ok(appender) => Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_thread_names(true)
                .with_writer(appender),
        ),
        Err(error) => {
            eprintln!("cannot open the log folder {}: {error}", logs_dir.display());
            None
        }
    };
    let stderr_layer =
        cfg!(debug_assertions).then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));

    let installed = tracing_subscriber::registry()
        .with(file_layer.with_filter(filter.clone()))
        .with(stderr_layer.with_filter(filter))
        .try_init();
    if let Err(error) = installed {
        eprintln!("cannot install the logger: {error}");
        return;
    }

    install_panic_hook();
    spawn_pruning(logs_dir.to_path_buf());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        logs = %logs_dir.display(),
        "Tan started"
    );
}

fn open_appender(logs_dir: &Path) -> Result<RollingFileAppender, String> {
    std::fs::create_dir_all(logs_dir).map_err(|error| error.to_string())?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .filename_suffix(FILE_SUFFIX)
        .build(logs_dir)
        .map_err(|error| error.to_string())
}

/// Logs every panic (message, location, backtrace) before the default hook runs.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        let location = info
            .location()
            .map_or_else(|| "unknown".to_owned(), ToString::to_string);
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(%thread, %location, %backtrace, "panic: {message}");
        previous(info);
    }));
}

fn spawn_pruning(logs_dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("log-retention".to_owned())
        .spawn(move || loop {
            match prune_old_logs(&logs_dir, LOG_MAX_AGE, SystemTime::now()) {
                Ok(0) => {}
                Ok(deleted) => tracing::info!(deleted, "deleted old log files"),
                Err(error) => tracing::warn!(%error, "cannot delete old log files"),
            }
            std::thread::sleep(PRUNE_INTERVAL);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot start the log retention thread");
    }
}

/// Deletes the Tan log files (`tan.*.log`) in `logs_dir` last written more than `max_age`
/// before `now`, and returns how many were deleted. Other files are left alone; a file that
/// cannot be inspected or deleted is skipped and logged.
///
/// # Errors
///
/// Returns an error if the folder cannot be read (a missing folder is not an error).
pub fn prune_old_logs(logs_dir: &Path, max_age: Duration, now: SystemTime) -> io::Result<usize> {
    let entries = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut deleted = 0;
    for entry in entries {
        let entry = entry?;
        if !is_log_file(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|meta| {
                if meta.is_file() {
                    meta.modified().map(Some)
                } else {
                    Ok(None)
                }
            })
            .map(|modified| {
                modified.is_some_and(|m| now.duration_since(m).is_ok_and(|age| age > max_age))
            });
        match expired {
            Ok(false) => {}
            Ok(true) => match std::fs::remove_file(entry.path()) {
                Ok(()) => deleted += 1,
                Err(error) => {
                    tracing::warn!(%error, file = %entry.path().display(), "cannot delete a log file");
                }
            },
            Err(error) => {
                tracing::warn!(%error, file = %entry.path().display(), "cannot inspect a log file");
            }
        }
    }
    Ok(deleted)
}

fn is_log_file(name: &str) -> bool {
    name.strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
        .is_some_and(|middle| middle.ends_with('.'))
}
