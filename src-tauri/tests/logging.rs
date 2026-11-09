//! Retention of the diagnostic log files.

use std::fs::File;
use std::time::{Duration, SystemTime};

use tan_app_lib::logging::{logs_dir, prune_old_logs, LOG_MAX_AGE};

const DAY: Duration = Duration::from_hours(24);

fn write_file(dir: &std::path::Path, name: &str, modified: SystemTime) {
    let file = File::create(dir.join(name)).unwrap();
    file.set_modified(modified).unwrap();
}

#[test]
fn logs_live_in_the_logs_folder_of_the_data_directory() {
    let root = std::path::Path::new("data");
    assert_eq!(logs_dir(root), root.join("logs"));
}

#[test]
fn log_files_older_than_the_limit_are_deleted_and_others_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    write_file(dir.path(), "tan.2026-01-01.log", now - LOG_MAX_AGE - DAY);
    write_file(dir.path(), "tan.2026-11-01.log", now - LOG_MAX_AGE + DAY);
    write_file(dir.path(), "tan.2026-11-09.log", now);
    // Not ours: never touched, however old.
    write_file(dir.path(), "notes.txt", now - LOG_MAX_AGE - DAY);
    write_file(dir.path(), "tan.log.bak", now - LOG_MAX_AGE - DAY);

    let deleted = prune_old_logs(dir.path(), LOG_MAX_AGE, now).unwrap();

    assert_eq!(deleted, 1);
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "notes.txt",
            "tan.2026-11-01.log",
            "tan.2026-11-09.log",
            "tan.log.bak"
        ]
    );
}

#[test]
fn a_missing_logs_folder_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("logs");
    assert_eq!(
        prune_old_logs(&missing, LOG_MAX_AGE, SystemTime::now()).unwrap(),
        0
    );
}

#[test]
fn the_limit_is_forty_five_days() {
    assert_eq!(LOG_MAX_AGE, 45 * DAY);
}

#[test]
fn init_writes_timestamped_lines_to_a_daily_file() {
    let dir = tempfile::tempdir().unwrap();
    let logs = logs_dir(dir.path());
    tan_app_lib::logging::init(&logs);
    tracing::warn!(probe = 42, "logging probe");

    let files: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("tan.") && files[0].extension().is_some_and(|e| e == "log"),
        "{name}"
    );
    let content = std::fs::read_to_string(&files[0]).unwrap();
    let line = content
        .lines()
        .find(|l| l.contains("logging probe"))
        .unwrap();
    // `2026-11-09T10:17:43.537Z  WARN ...`: an RFC 3339 UTC timestamp leads every line.
    assert!(line.as_bytes()[4] == b'-' && line.contains('T'), "{line}");
    assert!(line.contains("WARN") && line.contains("probe=42"), "{line}");
}
