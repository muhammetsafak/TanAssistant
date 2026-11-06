//! `TranscribeSession` against in-memory fakes of every port.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use tan_application::ports::{
    AiError, AiResult, AudioSource, ChatModel, ChatRequest, ChatResponse, ModelCatalog,
    ProviderFactory, ProviderProfileRepository, SessionRepository, TaskAssignmentRepository,
    TranscriptRepository, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
    VisionModel, VisionRequest,
};
use tan_application::transcription::{
    merge_tracks, AudioChunker, AudioPrepError, AudioSpan, LocalEngineStatus, LocalSpeed,
    LocalTranscriber, SessionAudioFiles, TranscribeError, TranscribePorts, TranscribeProgress,
    TranscribeSession, TranscriptionEngine, CLOUD_CHUNK_MS, SPEAKER_ME, SPEAKER_OTHERS,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    AudioTrack, EpochMs, LanguageCode, LanguageSetting, ProviderKind, ProviderProfile,
    ProviderProfileId, Session, SessionId, SessionKind, TaskAssignment, TaskKind, TrackKind,
    TranscriptSegment,
};

// ---- fakes -----------------------------------------------------------------------------

struct MemSessions {
    sessions: Vec<Session>,
    tracks: Vec<AudioTrack>,
}

impl SessionRepository for MemSessions {
    async fn create(&self, _session: &Session) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn update(&self, _session: &Session) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn get(&self, id: &SessionId) -> RepositoryResult<Option<Session>> {
        Ok(self.sessions.iter().find(|s| &s.id == id).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<Session>> {
        Ok(self.sessions.clone())
    }

    async fn delete(&self, _id: &SessionId) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn save_audio_track(&self, _track: &AudioTrack) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn list_audio_tracks(&self, session_id: &SessionId) -> RepositoryResult<Vec<AudioTrack>> {
        Ok(self
            .tracks
            .iter()
            .filter(|t| &t.session_id == session_id)
            .cloned()
            .collect())
    }
}

#[derive(Default)]
struct MemTranscripts {
    stored: Mutex<BTreeMap<String, Vec<TranscriptSegment>>>,
}

impl MemTranscripts {
    fn get_now(&self, session: &str) -> Option<Vec<TranscriptSegment>> {
        self.stored.lock().unwrap().get(session).cloned()
    }
}

impl TranscriptRepository for MemTranscripts {
    async fn replace_for_session(
        &self,
        session_id: &SessionId,
        segments: &[TranscriptSegment],
    ) -> RepositoryResult<()> {
        self.stored
            .lock()
            .unwrap()
            .insert(session_id.to_string(), segments.to_vec());
        Ok(())
    }

    async fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> RepositoryResult<Vec<TranscriptSegment>> {
        Ok(self.get_now(session_id.as_str()).unwrap_or_default())
    }
}

#[derive(Default)]
struct MemConfig {
    profiles: Vec<ProviderProfile>,
    assignment: Option<TaskAssignment>,
}

impl ProviderProfileRepository for MemConfig {
    async fn upsert(&self, _profile: &ProviderProfile) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn get(&self, id: &ProviderProfileId) -> RepositoryResult<Option<ProviderProfile>> {
        Ok(self.profiles.iter().find(|p| &p.id == id).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<ProviderProfile>> {
        Ok(self.profiles.clone())
    }

    async fn delete(&self, _id: &ProviderProfileId) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }
}

impl TaskAssignmentRepository for MemConfig {
    async fn set(&self, _assignment: &TaskAssignment) -> RepositoryResult<()> {
        Err(RepositoryError::Invalid("not used".to_owned()))
    }

    async fn get(&self, task: TaskKind) -> RepositoryResult<Option<TaskAssignment>> {
        Ok(self
            .assignment
            .clone()
            .filter(|assignment| assignment.task == task))
    }

    async fn list(&self) -> RepositoryResult<Vec<TaskAssignment>> {
        Ok(self.assignment.clone().into_iter().collect())
    }

    async fn clear(&self, _task: TaskKind) -> RepositoryResult<()> {
        Ok(())
    }
}

/// What a fake engine saw and what it answers per call.
#[derive(Default)]
struct EngineState {
    requests: Mutex<Vec<TranscriptionRequest>>,
    answers: Mutex<Vec<AiResult<Vec<TranscriptSegmentDto>>>>,
}

#[derive(Clone, Default)]
struct FakeEngine {
    state: Arc<EngineState>,
}

impl FakeEngine {
    fn answering(answers: Vec<AiResult<Vec<TranscriptSegmentDto>>>) -> Self {
        let engine = Self::default();
        *engine.state.answers.lock().unwrap() = answers;
        engine
    }

    fn requests(&self) -> Vec<TranscriptionRequest> {
        self.state.requests.lock().unwrap().clone()
    }
}

impl TranscriptionModel for FakeEngine {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        self.state.requests.lock().unwrap().push(request);
        let mut answers = self.state.answers.lock().unwrap();
        if answers.is_empty() {
            Ok(Vec::new())
        } else {
            answers.remove(0)
        }
    }
}

struct FakeLocal {
    engine: FakeEngine,
    status: LocalEngineStatus,
}

impl TranscriptionModel for FakeLocal {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        self.engine.transcribe(request).await
    }
}

impl LocalTranscriber for FakeLocal {
    async fn status(&self) -> LocalEngineStatus {
        self.status
    }

    async fn speed(&self) -> Result<LocalSpeed, AiError> {
        Ok(LocalSpeed {
            seconds_per_window: 12.5,
            backend: "fake-gpu".to_owned(),
        })
    }

    // Every file "holds" 65 s of speech: three whisper windows.
    async fn speech_ms(&self, _audio: &Path) -> Result<u64, AudioPrepError> {
        Ok(65_000)
    }
}

/// Cloud adapter factory: hands out the scripted engine; chat/vision/catalog are not used.
struct FakeFactory(FakeEngine);

impl ChatModel for FakeEngine {
    async fn complete(&self, _request: ChatRequest) -> AiResult<ChatResponse> {
        Err(AiError::Unsupported("not used".to_owned()))
    }
}

impl VisionModel for FakeEngine {
    async fn describe(&self, _request: VisionRequest) -> AiResult<ChatResponse> {
        Err(AiError::Unsupported("not used".to_owned()))
    }
}

impl ModelCatalog for FakeEngine {
    async fn list_models(&self) -> AiResult<Vec<String>> {
        Err(AiError::Unsupported("not used".to_owned()))
    }
}

impl ProviderFactory for FakeFactory {
    type Chat = FakeEngine;
    type Vision = FakeEngine;
    type Transcription = FakeEngine;
    type Catalog = FakeEngine;

    async fn chat(&self, _profile: &ProviderProfile) -> AiResult<FakeEngine> {
        Err(AiError::Unsupported("not used".to_owned()))
    }

    async fn vision(&self, _profile: &ProviderProfile) -> AiResult<FakeEngine> {
        Err(AiError::Unsupported("not used".to_owned()))
    }

    async fn transcription(&self, _profile: &ProviderProfile) -> AiResult<FakeEngine> {
        Ok(self.0.clone())
    }

    async fn catalog(&self, _profile: &ProviderProfile) -> AiResult<FakeEngine> {
        Err(AiError::Unsupported("not used".to_owned()))
    }
}

/// Plans two 6-minute-style pieces for any file longer than nothing; loads a tagged blob.
struct FakeChunker {
    spans: Vec<AudioSpan>,
}

impl AudioChunker for FakeChunker {
    async fn plan(
        &self,
        _audio: &Path,
        max_chunk_ms: u64,
    ) -> Result<Vec<AudioSpan>, AudioPrepError> {
        assert_eq!(max_chunk_ms, CLOUD_CHUNK_MS);
        Ok(self.spans.clone())
    }

    async fn load_wav(&self, audio: &Path, span: AudioSpan) -> Result<Vec<u8>, AudioPrepError> {
        Ok(format!("{}@{}", audio.display(), span.start_ms).into_bytes())
    }
}

struct FakeFiles;

impl SessionAudioFiles for FakeFiles {
    fn audio_path(&self, session_id: &SessionId, relative_path: &str) -> io::Result<PathBuf> {
        Ok(PathBuf::from(format!("/data/{session_id}/{relative_path}")))
    }
}

// ---- helpers ---------------------------------------------------------------------------

type Subject = TranscribeSession<
    MemSessions,
    MemTranscripts,
    MemConfig,
    MemConfig,
    FakeFactory,
    FakeLocal,
    FakeChunker,
    FakeFiles,
>;

struct Rig {
    subject: Subject,
    transcripts: Arc<MemTranscripts>,
    local: FakeEngine,
    cloud: FakeEngine,
}

fn session(id: &str, language: LanguageSetting) -> Session {
    Session::start_recording(
        SessionId::new(id),
        SessionKind::Meeting,
        "Weekly sync",
        EpochMs::new(0),
        language,
        LanguageCode::turkish(),
    )
}

fn track(session: &str, kind: TrackKind) -> AudioTrack {
    AudioTrack {
        session_id: SessionId::new(session),
        kind,
        path: format!("audio/{kind}.wav"),
        sample_rate: 48_000,
        duration_ms: 60_000,
    }
}

fn dto(start_ms: u64, end_ms: u64, text: &str) -> TranscriptSegmentDto {
    TranscriptSegmentDto {
        start_ms,
        end_ms,
        text: text.to_owned(),
        language: None,
    }
}

struct RigBuilder {
    sessions: Vec<Session>,
    tracks: Vec<AudioTrack>,
    local_answers: Vec<AiResult<Vec<TranscriptSegmentDto>>>,
    cloud_answers: Vec<AiResult<Vec<TranscriptSegmentDto>>>,
    local_status: LocalEngineStatus,
    profiles: Vec<ProviderProfile>,
    assignment: Option<TaskAssignment>,
    spans: Vec<AudioSpan>,
}

impl RigBuilder {
    fn new() -> Self {
        Self {
            sessions: vec![session(
                "s1",
                LanguageSetting::Fixed(LanguageCode::turkish()),
            )],
            tracks: vec![
                track("s1", TrackKind::System),
                track("s1", TrackKind::Microphone),
            ],
            local_answers: Vec::new(),
            cloud_answers: Vec::new(),
            local_status: LocalEngineStatus::Ready,
            profiles: Vec::new(),
            assignment: None,
            spans: vec![AudioSpan {
                start_ms: 0,
                end_ms: 60_000,
            }],
        }
    }

    fn assign(mut self, kind: ProviderKind) -> Self {
        let profile = ProviderProfile {
            id: ProviderProfileId::new("p1"),
            kind,
            display_name: "Cloud".to_owned(),
            model: "whisper-1".to_owned(),
            base_url: None,
            secret_ref: None,
            supports_vision: false,
        };
        self.assignment = Some(TaskAssignment {
            task: TaskKind::Transcription,
            profile_id: profile.id.clone(),
        });
        self.profiles.push(profile);
        self
    }

    fn build(self) -> Rig {
        let transcripts = Arc::new(MemTranscripts::default());
        let local = FakeEngine::answering(self.local_answers);
        let cloud = FakeEngine::answering(self.cloud_answers);
        let config = Arc::new(MemConfig {
            profiles: self.profiles,
            assignment: self.assignment,
        });
        let subject = TranscribeSession::new(TranscribePorts {
            sessions: Arc::new(MemSessions {
                sessions: self.sessions,
                tracks: self.tracks,
            }),
            transcripts: transcripts.clone(),
            assignments: config.clone(),
            profiles: config,
            factory: Arc::new(FakeFactory(cloud.clone())),
            local: Arc::new(FakeLocal {
                engine: local.clone(),
                status: self.local_status,
            }),
            chunker: Arc::new(FakeChunker { spans: self.spans }),
            files: Arc::new(FakeFiles),
        });
        Rig {
            subject,
            transcripts,
            local,
            cloud,
        }
    }
}

// ---- tests -----------------------------------------------------------------------------

#[test]
fn without_an_assignment_local_whisper_is_used() {
    let mut builder = RigBuilder::new();
    // Tracks are processed microphone first, then system.
    builder.local_answers = vec![
        Ok(vec![dto(1_000, 2_000, " Merhaba ")]),
        Ok(vec![dto(500, 900, "Selam")]),
    ];
    let rig = builder.build();

    let summary = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(summary.engine, TranscriptionEngine::LocalWhisper);
    assert_eq!(summary.segment_count, 2);
    assert!(rig.cloud.requests().is_empty(), "no provider call");
    let requests = rig.local.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        matches!(&requests[0].audio, AudioSource::Path(p) if p.ends_with("microphone.wav")),
        "local whisper reads the file itself"
    );
    assert_eq!(requests[0].language, Some(LanguageCode::turkish()));
    assert_eq!(requests[0].duration_ms, Some(60_000));
}

#[test]
fn the_two_tracks_are_merged_by_time_with_speaker_labels() {
    let mut builder = RigBuilder::new();
    builder.local_answers = vec![
        // microphone
        Ok(vec![dto(0, 1_000, "one"), dto(4_000, 5_000, "four")]),
        // system
        Ok(vec![dto(2_000, 3_000, "two"), dto(6_000, 7_000, "six")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    let stored = rig.transcripts.get_now("s1").unwrap();
    let texts: Vec<_> = stored.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["one", "two", "four", "six"]);
    let speakers: Vec<_> = stored
        .iter()
        .map(|s| s.speaker.as_deref().unwrap())
        .collect();
    assert_eq!(
        speakers,
        [SPEAKER_ME, SPEAKER_OTHERS, SPEAKER_ME, SPEAKER_OTHERS]
    );
    let tracks: Vec<_> = stored.iter().map(|s| s.track).collect();
    assert_eq!(
        tracks,
        [
            TrackKind::Microphone,
            TrackKind::System,
            TrackKind::Microphone,
            TrackKind::System
        ]
    );
    assert!(stored.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
}

#[test]
fn an_assigned_provider_is_used_instead_of_local_whisper() {
    let mut builder = RigBuilder::new().assign(ProviderKind::OpenAi);
    builder.cloud_answers = vec![Ok(vec![dto(0, 500, "hello")]), Ok(vec![dto(0, 500, "hi")])];
    let rig = builder.build();

    let summary = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(
        summary.engine,
        TranscriptionEngine::Provider {
            profile_id: ProviderProfileId::new("p1"),
            kind: ProviderKind::OpenAi
        }
    );
    assert!(rig.local.requests().is_empty(), "local whisper not touched");
    let requests = rig.cloud.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        matches!(&requests[0].audio, AudioSource::Bytes { mime_type, .. } if mime_type == "audio/wav"),
        "providers get an uploadable WAV, not a path"
    );
}

#[test]
fn long_audio_is_uploaded_in_pieces_and_shifted_back_onto_the_file_timeline() {
    let mut builder = RigBuilder::new().assign(ProviderKind::Gemini);
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    builder.spans = vec![
        AudioSpan {
            start_ms: 0,
            end_ms: 360_000,
        },
        AudioSpan {
            start_ms: 360_000,
            end_ms: 500_000,
        },
    ];
    builder.cloud_answers = vec![
        Ok(vec![dto(1_000, 2_000, "first piece")]),
        Ok(vec![dto(1_000, 2_000, "second piece")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    let stored = rig.transcripts.get_now("s1").unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!((stored[0].start_ms, stored[0].end_ms), (1_000, 2_000));
    assert_eq!((stored[1].start_ms, stored[1].end_ms), (361_000, 362_000));
    let durations: Vec<_> = rig
        .cloud
        .requests()
        .iter()
        .map(|r| r.duration_ms.unwrap())
        .collect();
    assert_eq!(durations, [360_000, 140_000]);
}

#[test]
fn a_vanished_profile_falls_back_to_local_whisper() {
    let mut builder = RigBuilder::new().assign(ProviderKind::OpenAi);
    builder.profiles.clear();
    builder.local_answers = vec![Ok(vec![dto(0, 100, "x")]), Ok(Vec::new())];
    let rig = builder.build();

    let summary = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(summary.engine, TranscriptionEngine::LocalWhisper);
}

#[test]
fn a_provider_without_transcription_is_refused() {
    let rig = RigBuilder::new().assign(ProviderKind::Anthropic).build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(matches!(
        error,
        TranscribeError::Provider(AiError::Unsupported(_))
    ));
    assert!(rig.transcripts.get_now("s1").is_none());
}

#[test]
fn a_missing_local_model_is_a_typed_error_and_nothing_runs() {
    let mut builder = RigBuilder::new();
    builder.local_status = LocalEngineStatus::ModelMissing;
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(matches!(error, TranscribeError::LocalModelMissing));
    assert_eq!(rig.local.requests().len(), 0);
    assert!(rig.transcripts.get_now("s1").is_none());
}

#[test]
fn a_failing_track_keeps_the_tracks_that_finished() {
    let mut builder = RigBuilder::new();
    builder.local_answers = vec![Ok(vec![dto(0, 100, "ok")]), Err(AiError::Timeout)];
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(matches!(
        error,
        TranscribeError::Engine {
            track: TrackKind::System,
            error: AiError::Timeout
        }
    ));
    // The microphone track was done before the system track failed: its work is stored.
    let stored = rig.transcripts.get_now("s1").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].track, TrackKind::Microphone);
}

#[test]
fn a_rerun_that_fails_half_way_keeps_the_earlier_complete_transcript() {
    let mut builder = RigBuilder::new();
    builder.local_answers = vec![
        Ok(vec![dto(0, 100, "old me")]),
        Ok(vec![dto(0, 100, "old others")]),
        Ok(vec![dto(0, 100, "new me")]),
        Err(AiError::Timeout),
    ];
    let rig = builder.build();
    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    // Second run: the microphone track finishes, the system track fails.
    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(matches!(error, TranscribeError::Engine { .. }), "{error:?}");
    let stored = rig.transcripts.get_now("s1").unwrap();
    let texts: Vec<_> = stored.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["old me", "old others"]);
}

#[test]
fn progress_is_reported_before_each_track_and_at_the_end() {
    let mut builder = RigBuilder::new();
    builder.local_answers = vec![Ok(vec![dto(0, 100, "a")]), Ok(vec![dto(0, 100, "b")])];
    let rig = builder.build();
    let seen = Mutex::new(Vec::new());

    block_on(
        rig.subject
            .execute_with_progress(&SessionId::new("s1"), &|p| seen.lock().unwrap().push(p)),
    )
    .unwrap();

    let seen = seen.into_inner().unwrap();
    let progress = |track, tracks_done| TranscribeProgress {
        track,
        tracks_done,
        tracks_total: 2,
    };
    assert_eq!(
        seen,
        [
            progress(Some(TrackKind::Microphone), 0),
            progress(Some(TrackKind::System), 1),
            progress(None, 2)
        ]
    );
}

#[test]
fn the_local_estimate_counts_whisper_windows_of_speech_times_the_calibrated_speed() {
    let rig = RigBuilder::new().build();

    let estimate = block_on(rig.subject.estimate(&SessionId::new("s1"))).unwrap();

    // Two tracks x 65 s of speech = 2 x 3 windows x 12.5 s.
    assert_eq!(estimate.engine, TranscriptionEngine::LocalWhisper);
    assert_eq!(estimate.estimated_seconds, Some(75));
    assert_eq!(estimate.backend, "fake-gpu");
    assert_eq!(estimate.speech_ms, Some(130_000));
    assert_eq!(estimate.audio_ms, 120_000);
    // Estimating does not transcribe anything.
    assert_eq!(rig.local.requests().len(), 0);
    assert!(rig.transcripts.get_now("s1").is_none());
}

#[test]
fn a_cloud_estimate_has_no_time_and_a_missing_model_is_refused() {
    let rig = RigBuilder::new().assign(ProviderKind::OpenAi).build();
    let estimate = block_on(rig.subject.estimate(&SessionId::new("s1"))).unwrap();
    assert_eq!(estimate.estimated_seconds, None);
    assert_eq!(estimate.backend, "cloud");
    assert_eq!(estimate.speech_ms, None);

    let mut builder = RigBuilder::new();
    builder.local_status = LocalEngineStatus::ModelMissing;
    let rig = builder.build();
    assert!(matches!(
        block_on(rig.subject.estimate(&SessionId::new("s1"))),
        Err(TranscribeError::LocalModelMissing)
    ));
}

#[test]
fn a_session_without_audio_or_unknown_is_rejected() {
    let mut builder = RigBuilder::new();
    builder.tracks.clear();
    let rig = builder.build();
    assert!(matches!(
        block_on(rig.subject.execute(&SessionId::new("s1"))),
        Err(TranscribeError::NoAudio)
    ));
    assert!(matches!(
        block_on(rig.subject.execute(&SessionId::new("nope"))),
        Err(TranscribeError::NotFound)
    ));
}

#[test]
fn empty_text_is_dropped_and_the_session_language_fills_a_missing_one() {
    let mut builder = RigBuilder::new();
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    let detected = TranscriptSegmentDto {
        language: Some(LanguageCode::english()),
        ..dto(2_000, 3_000, "hello")
    };
    builder.local_answers = vec![Ok(vec![
        dto(0, 1_000, "   "),
        dto(1_000, 2_000, "merhaba"),
        detected,
    ])];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    let stored = rig.transcripts.get_now("s1").unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].language, Some(LanguageCode::turkish()));
    assert_eq!(stored[1].language, Some(LanguageCode::english()));
}

#[test]
fn an_auto_language_session_asks_the_engine_to_detect() {
    let mut builder = RigBuilder::new();
    builder.sessions = vec![session("s1", LanguageSetting::Auto)];
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    builder.local_answers = vec![Ok(vec![dto(0, 100, "merhaba")])];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(rig.local.requests()[0].language, None);
    assert_eq!(rig.transcripts.get_now("s1").unwrap()[0].language, None);
}

#[test]
fn a_rerun_replaces_the_previous_transcript() {
    let mut builder = RigBuilder::new();
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    builder.local_answers = vec![
        Ok(vec![dto(0, 100, "first")]),
        Ok(vec![dto(0, 100, "second")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();
    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    let stored = rig.transcripts.get_now("s1").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].text, "second");
}

#[test]
fn merge_is_deterministic_when_segments_start_together() {
    let segment = |track, start, end, text: &str| TranscriptSegment {
        session_id: SessionId::new("s1"),
        track,
        start_ms: start,
        end_ms: end,
        language: None,
        text: text.to_owned(),
        speaker: None,
    };
    let merged = merge_tracks(vec![
        segment(TrackKind::System, 0, 500, "system"),
        segment(TrackKind::Microphone, 0, 500, "mic"),
        segment(TrackKind::System, 0, 200, "short"),
    ]);
    let texts: Vec<_> = merged.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["short", "mic", "system"]);
}
