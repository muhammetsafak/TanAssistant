//! Shared helpers for the integration tests. Compiled as its own (empty) test binary too,
//! hence the `dead_code` allowance.
#![allow(dead_code, clippy::must_use_candidate)]

use std::future::Future;
use std::path::PathBuf;

use tan_domain::{
    EpochMs, LanguageCode, LanguageSetting, Session, SessionId, SessionKind, TrackKind,
    TranscriptSegment,
};
use tan_infra_storage::Database;
use tempfile::TempDir;

pub fn block_on<F: Future>(future: F) -> F::Output {
    futures_executor::block_on(future)
}

/// A database file inside a temporary directory that is removed on drop.
pub struct TestDb {
    pub dir: TempDir,
    pub path: PathBuf,
    pub db: Database,
}

pub fn test_db() -> TestDb {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("data").join("tan.db");
    let db = Database::open(&path).expect("open database");
    TestDb { dir, path, db }
}

pub fn session(id: &str, started_at: i64) -> Session {
    Session::start_recording(
        SessionId::new(id),
        SessionKind::Meeting,
        format!("Meeting {id}"),
        EpochMs::new(started_at),
        LanguageSetting::Auto,
        LanguageCode::turkish(),
    )
}

pub fn segment(session_id: &str, start_ms: u64, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        session_id: SessionId::new(session_id),
        track: TrackKind::System,
        start_ms,
        end_ms: start_ms + 1_000,
        language: Some(LanguageCode::turkish()),
        text: text.to_owned(),
        speaker: Some("Speaker 1".to_owned()),
    }
}
