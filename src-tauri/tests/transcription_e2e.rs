//! End to end: a recorded session (two WAV tracks on disk, rows in SQLite) is transcribed with
//! real local whisper and the transcript lands in the database and the search index; the
//! forecast for it is sane.
//!
//! Needs a ggml model: set `TAN_WHISPER_MODEL=<path>`; without it the test does nothing. It
//! lives here, at the composition root, because it wires real adapters from several infra
//! crates. Not run in CI.
// The wiring below needs a provider factory that is never used (no cloud assignment).
#![allow(clippy::unused_async_trait_impl, clippy::cast_possible_truncation)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, ModelCatalog, ProviderFactory,
    SearchIndex, SessionRepository, TranscriptRepository, TranscriptSegmentDto, TranscriptionModel,
    TranscriptionRequest, VisionModel, VisionRequest,
};
use tan_application::transcription::{
    TranscribePorts, TranscribeSession, TranscriptionEngine, SPEAKER_ME, SPEAKER_OTHERS,
};
use tan_domain::{
    AudioTrack, EpochMs, LanguageCode, LanguageSetting, ProviderProfile, Session, SessionId,
    SessionKind, TrackKind,
};
use tan_infra_storage::{
    DataLayout, Database, SqliteProviderProfileRepository, SqliteSearchIndex,
    SqliteSessionRepository, SqliteTaskAssignmentRepository, SqliteTranscriptRepository,
};
use tan_infra_transcription::{SessionFiles, WavAudioChunker, WhisperTranscriber};

#[derive(Clone)]
struct Unused;

impl ChatModel for Unused {
    async fn complete(&self, _: ChatRequest) -> AiResult<ChatResponse> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
}
impl VisionModel for Unused {
    async fn describe(&self, _: VisionRequest) -> AiResult<ChatResponse> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
}
impl TranscriptionModel for Unused {
    async fn transcribe(&self, _: TranscriptionRequest) -> AiResult<Vec<TranscriptSegmentDto>> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
}
impl ModelCatalog for Unused {
    async fn list_models(&self) -> AiResult<Vec<String>> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
}

struct NoProviders;

impl ProviderFactory for NoProviders {
    type Chat = Unused;
    type Vision = Unused;
    type Transcription = Unused;
    type Catalog = Unused;

    async fn chat(&self, _: &ProviderProfile) -> AiResult<Unused> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
    async fn vision(&self, _: &ProviderProfile) -> AiResult<Unused> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
    async fn transcription(&self, _: &ProviderProfile) -> AiResult<Unused> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
    async fn catalog(&self, _: &ProviderProfile) -> AiResult<Unused> {
        Err(AiError::Unsupported("unused".to_owned()))
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("crates")
        .join("infra-transcription")
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Writes a 16 kHz mono fixture as a 48 kHz stereo track, like the recorder stores it, with
/// `lead_ms` of silence before and `tail_ms` after. Returns the track's duration.
fn write_native_track(source: &Path, target: &Path, lead_ms: u64, tail_ms: u64) -> u64 {
    let mut reader = hound::WavReader::open(source).unwrap();
    let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(target, spec).unwrap();
    let silence = |writer: &mut hound::WavWriter<_>, ms: u64| {
        for _ in 0..ms * 48 {
            writer.write_sample(0_i16).unwrap();
            writer.write_sample(0_i16).unwrap();
        }
    };
    silence(&mut writer, lead_ms);
    for pair in samples.windows(2) {
        let (a, b) = (i32::from(pair[0]), i32::from(pair[1]));
        for step in 0..3 {
            let value = (a + (b - a) * step / 3) as i16;
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
    }
    silence(&mut writer, tail_ms);
    writer.finalize().unwrap();
    lead_ms + (samples.len() as u64 - 1) * 1_000 / 16_000 + tail_ms
}

#[test]
fn a_recorded_session_is_transcribed_into_the_database() {
    let Some(model) = std::env::var_os("TAN_WHISPER_MODEL") else {
        eprintln!("TAN_WHISPER_MODEL is not set: skipping the end-to-end test");
        return;
    };
    tauri::async_runtime::block_on(run(model));
}

async fn run(model: std::ffi::OsString) {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path());
    let db = Database::open(&layout.database_path()).unwrap();
    let sessions = Arc::new(SqliteSessionRepository::new(db.clone()));
    let transcripts = Arc::new(SqliteTranscriptRepository::new(db.clone()));
    let search = SqliteSearchIndex::new(db.clone());

    // A finished recording: Turkish speech on the microphone after 10 s of silence (and 10 s
    // after it), English on the system track.
    let id = SessionId::new("0190-e2e");
    let mut session = Session::start_recording(
        id.clone(),
        SessionKind::Meeting,
        "Weekly sync",
        EpochMs::new(1_000),
        LanguageSetting::Auto,
        LanguageCode::turkish(),
    );
    session.finish_recording(EpochMs::new(30_000)).unwrap();
    sessions.create(&session).await.unwrap();
    layout.ensure_session_dirs(&id).unwrap();
    for (kind, fixture_name, lead_ms, tail_ms) in [
        (TrackKind::Microphone, "speech_tr.wav", 10_000, 10_000),
        (TrackKind::System, "speech_en.wav", 0, 0),
    ] {
        let relative = format!("audio/{kind}.wav");
        let target = layout.session_dir(&id).unwrap().join(&relative);
        let duration_ms = write_native_track(&fixture(fixture_name), &target, lead_ms, tail_ms);
        sessions
            .save_audio_track(&AudioTrack {
                session_id: id.clone(),
                kind,
                path: relative,
                sample_rate: 48_000,
                duration_ms,
            })
            .await
            .unwrap();
    }

    let use_case = TranscribeSession::new(TranscribePorts {
        sessions: sessions.clone(),
        transcripts: transcripts.clone(),
        assignments: Arc::new(SqliteTaskAssignmentRepository::new(db.clone())),
        profiles: Arc::new(SqliteProviderProfileRepository::new(db)),
        factory: Arc::new(NoProviders),
        local: Arc::new(WhisperTranscriber::with_model_file(model)),
        chunker: Arc::new(WavAudioChunker),
        files: Arc::new(SessionFiles::new(layout.sessions_dir())),
    });

    // The forecast: local engine, 5 s of speech per track at most 7 s long -> one window each.
    let estimate = use_case.estimate(&id).await.unwrap();
    println!("estimate: {estimate:?}");
    assert_eq!(estimate.engine, TranscriptionEngine::LocalWhisper);
    assert!(estimate.estimated_seconds.is_some_and(|s| s >= 2));
    let speech_ms = estimate.speech_ms.unwrap();
    assert!(
        (6_000..=16_000).contains(&speech_ms),
        "the 20 s of silence around the speech must not count: {speech_ms} ms of speech"
    );
    assert!(estimate.audio_ms > 30_000);

    let summary = use_case.execute(&id).await.unwrap();

    assert_eq!(summary.engine, TranscriptionEngine::LocalWhisper);
    let stored = transcripts.list_for_session(&id).await.unwrap();
    assert_eq!(stored.len(), summary.segment_count);
    assert_ne!(stored.len(), 0);
    let text_of = |speaker: &str| {
        stored
            .iter()
            .filter(|s| s.speaker.as_deref() == Some(speaker))
            .map(|s| s.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mine = text_of(SPEAKER_ME);
    let theirs = text_of(SPEAKER_OTHERS);
    println!("me: {mine:?}\nothers: {theirs:?}");
    assert!(mine.contains("toplant"), "{mine}");
    assert!(theirs.contains("budget"), "{theirs}");
    assert!(stored.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    // Auto language detection tagged each segment with what was spoken.
    assert!(stored
        .iter()
        .filter(|s| s.track == TrackKind::Microphone)
        .all(|s| s.language == Some(LanguageCode::turkish())));
    // The silence was skipped, yet the times are the recording's: the speech starts after the
    // 10 s lead-in and ends before the 10 s tail.
    for segment in stored.iter().filter(|s| s.track == TrackKind::Microphone) {
        assert!(
            (9_000..=17_500).contains(&segment.start_ms) && segment.end_ms <= 17_800,
            "{segment:?}"
        );
    }

    // The transcript is searchable.
    let hits = search.search("toplantı", 10).await.unwrap();
    assert!(hits.iter().any(|h| h.session_id == id), "{hits:?}");
}
