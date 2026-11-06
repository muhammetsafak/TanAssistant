//! `TranscribeSession` against in-memory fakes of every port.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use tan_application::ports::{
    AiError, AiResult, AudioSource, ChatModel, ChatRequest, ChatResponse, ModelCatalog,
    ProviderFactory, ProviderProfileRepository, SessionRepository, TaskAssignmentRepository,
    TranscriptRepository, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
    VisionModel, VisionRequest,
};
use tan_application::transcription::{
    merge_tracks, AudioChunker, AudioPrepError, AudioSpan, ChunkProgress, ChunkSettings,
    EncodedAudio, LocalEngineStatus, LocalSpeed, LocalTranscriber, RetryPolicy, SessionAudioFiles,
    SpeechMap, TranscribeError, TranscribePorts, TranscribeProgress, TranscribeSession,
    TranscriptionEngine, UploadFormat, SPEAKER_ME, SPEAKER_OTHERS,
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
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

/// Returns `Pending` once, so other requests get to start before this one finishes.
struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
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

    fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }
}

impl TranscriptionModel for FakeEngine {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        self.state.requests.lock().unwrap().push(request);
        let answer = {
            let mut answers = self.state.answers.lock().unwrap();
            if answers.is_empty() {
                Ok(Vec::new())
            } else {
                answers.remove(0)
            }
        };
        let now = self.state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.max_in_flight.fetch_max(now, Ordering::SeqCst);
        YieldOnce(false).await;
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        answer
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

/// Reports the configured spans as the speech of any file; encodes a tagged blob.
struct FakeChunker {
    spans: Vec<AudioSpan>,
    speech_maps: Arc<AtomicUsize>,
}

impl AudioChunker for FakeChunker {
    async fn speech_map(&self, _audio: &Path) -> Result<SpeechMap, AudioPrepError> {
        self.speech_maps.fetch_add(1, Ordering::SeqCst);
        Ok(SpeechMap {
            duration_ms: self.spans.last().map_or(0, |s| s.end_ms),
            regions: self.spans.clone(),
            energies: Vec::new(),
            energy_window_ms: 30,
        })
    }

    async fn encode(
        &self,
        audio: &Path,
        span: AudioSpan,
        format: UploadFormat,
    ) -> Result<EncodedAudio, AudioPrepError> {
        let (file_name, mime_type) = match format {
            UploadFormat::Compressed => ("audio.flac", "audio/flac"),
            UploadFormat::Wav => ("audio.wav", "audio/wav"),
        };
        Ok(EncodedAudio {
            file_name: file_name.to_owned(),
            mime_type: mime_type.to_owned(),
            data: format!("{}@{}", audio.display(), span.start_ms).into_bytes(),
        })
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
    speech_maps: Arc<AtomicUsize>,
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
    chunking: ChunkSettings,
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
                end_ms: 25_000,
            }],
            chunking: ChunkSettings {
                retry: RetryPolicy {
                    base_delay: std::time::Duration::ZERO,
                    ..RetryPolicy::default()
                },
                ..ChunkSettings::default()
            },
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
        let speech_maps = Arc::new(AtomicUsize::new(0));
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
            chunker: Arc::new(FakeChunker {
                spans: self.spans,
                speech_maps: speech_maps.clone(),
            }),
            files: Arc::new(FakeFiles),
        });
        let subject = subject.with_chunking(self.chunking);
        Rig {
            subject,
            transcripts,
            local,
            cloud,
            speech_maps,
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
        matches!(&requests[0].audio, AudioSource::Bytes { mime_type, .. } if mime_type == "audio/flac"),
        "providers get an uploadable compressed piece, not a path"
    );
}

#[test]
fn long_audio_is_uploaded_in_pieces_and_shifted_back_onto_the_file_timeline() {
    let mut builder = RigBuilder::new().assign(ProviderKind::Gemini);
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    builder.spans = vec![
        AudioSpan {
            start_ms: 0,
            end_ms: 20_000,
        },
        AudioSpan {
            start_ms: 360_000,
            end_ms: 380_000,
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
    assert_eq!(durations, [20_000, 20_000]);
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

fn cloud_profile(id: &str, kind: ProviderKind) -> ProviderProfile {
    ProviderProfile {
        id: ProviderProfileId::new(id),
        kind,
        display_name: id.to_owned(),
        model: "whisper-1".to_owned(),
        base_url: None,
        secret_ref: None,
        supports_vision: false,
    }
}

#[test]
fn a_profile_chosen_for_one_call_uses_the_cloud_without_assigning_anything() {
    // No assignment at all: the general engine is local whisper. (The assignments fake refuses
    // writes, so a call that tried to assign the profile would fail.)
    let mut builder = RigBuilder::new();
    builder
        .profiles
        .push(cloud_profile("one-off", ProviderKind::OpenAi));
    builder.cloud_answers = vec![Ok(vec![dto(0, 500, "hello")]), Ok(vec![dto(0, 500, "hi")])];
    builder.local_answers = vec![
        Ok(vec![dto(0, 500, "local")]),
        Ok(vec![dto(0, 500, "local")]),
    ];
    let rig = builder.build();
    let id = SessionId::new("s1");

    let summary = block_on(rig.subject.execute_with_profile(
        &id,
        Some(&ProviderProfileId::new("one-off")),
        &|_| {},
        &|_| {},
    ))
    .unwrap();
    assert_eq!(
        summary.engine,
        TranscriptionEngine::Provider {
            profile_id: ProviderProfileId::new("one-off"),
            kind: ProviderKind::OpenAi
        }
    );
    assert!(rig.local.requests().is_empty(), "local whisper not touched");
    assert_eq!(rig.cloud.requests().len(), 2);

    // The next plain call is back on the general engine.
    let summary = block_on(rig.subject.execute(&id)).unwrap();
    assert_eq!(summary.engine, TranscriptionEngine::LocalWhisper);
    assert_eq!(rig.cloud.requests().len(), 2, "no more cloud calls");
}

#[test]
fn a_chosen_profile_wins_over_the_assigned_one_and_must_exist_and_transcribe() {
    let mut builder = RigBuilder::new().assign(ProviderKind::OpenAi);
    builder
        .profiles
        .push(cloud_profile("claude", ProviderKind::Anthropic));
    builder
        .profiles
        .push(cloud_profile("other", ProviderKind::Gemini));
    builder.cloud_answers = vec![Ok(vec![dto(0, 500, "a")]), Ok(vec![dto(0, 500, "b")])];
    let rig = builder.build();
    let id = SessionId::new("s1");
    let run = |profile: &str| {
        block_on(rig.subject.execute_with_profile(
            &id,
            Some(&ProviderProfileId::new(profile)),
            &|_| {},
            &|_| {},
        ))
    };

    let error = run("missing").unwrap_err();
    assert!(
        matches!(error, TranscribeError::Provider(AiError::InvalidRequest(_))),
        "{error}"
    );
    let error = run("claude").unwrap_err();
    assert!(
        matches!(error, TranscribeError::Provider(AiError::Unsupported(_))),
        "{error}"
    );
    assert!(rig.transcripts.get_now("s1").is_none());

    let summary = run("other").unwrap();
    assert_eq!(
        summary.engine,
        TranscriptionEngine::Provider {
            profile_id: ProviderProfileId::new("other"),
            kind: ProviderKind::Gemini
        },
        "not the assigned p1"
    );
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

// ---- cloud transcription in pieces -----------------------------------------------------------

fn span(start_ms: u64, end_ms: u64) -> AudioSpan {
    AudioSpan { start_ms, end_ms }
}

/// A microphone-only session whose speech is `spans`, sent `concurrency` pieces at a time.
fn pieces_rig(spans: &[AudioSpan], concurrency: usize) -> RigBuilder {
    let mut builder = RigBuilder::new().assign(ProviderKind::OpenAi);
    builder.tracks = vec![track("s1", TrackKind::Microphone)];
    builder.spans = spans.to_vec();
    builder.chunking.concurrency = concurrency;
    builder
}

/// The start offset each request's tagged blob names, in request order.
fn request_offsets(engine: &FakeEngine) -> Vec<u64> {
    engine
        .requests()
        .iter()
        .map(|request| match &request.audio {
            AudioSource::Bytes { data, .. } => String::from_utf8_lossy(data)
                .rsplit('@')
                .next()
                .unwrap()
                .parse()
                .unwrap(),
            AudioSource::Path(_) => panic!("a piece must be uploaded as bytes"),
        })
        .collect()
}

fn stored_times(rig: &Rig) -> Vec<(u64, u64)> {
    rig.transcripts
        .get_now("s1")
        .unwrap()
        .iter()
        .map(|s| (s.start_ms, s.end_ms))
        .collect()
}

#[test]
fn a_model_without_timestamps_gives_each_piece_one_segment_at_the_pieces_own_time() {
    let spans = [
        span(0, 20_000),
        span(40_000, 50_000),
        span(100_000, 130_000),
    ];
    let mut builder = pieces_rig(&spans, 3);
    // What the adapters answer without timestamps: one segment over the piece (0..length).
    builder.cloud_answers = vec![
        Ok(vec![dto(0, 20_000, "first")]),
        Ok(vec![dto(0, 10_000, "second")]),
        Ok(vec![dto(0, 30_000, "third")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(
        stored_times(&rig),
        [(0, 20_000), (40_000, 50_000), (100_000, 130_000)]
    );
    let lengths: Vec<_> = rig
        .cloud
        .requests()
        .iter()
        .map(|r| r.duration_ms.unwrap())
        .collect();
    assert_eq!(lengths, [20_000, 10_000, 30_000]);
    assert_eq!(request_offsets(&rig.cloud), [0, 40_000, 100_000]);
}

#[test]
fn a_model_with_segment_times_is_shifted_by_each_pieces_offset_and_stays_monotonic() {
    let spans = [span(0, 20_000), span(40_000, 60_000), span(90_000, 110_000)];
    let mut builder = pieces_rig(&spans, 3);
    builder.cloud_answers = vec![
        // Out of order, and the last one overruns the piece: it is clamped to it.
        Ok(vec![
            dto(10_000, 15_000, "b"),
            dto(0, 9_000, "a"),
            dto(15_000, 99_000, "c"),
        ]),
        Ok(vec![dto(1_000, 2_000, "d"), dto(2_000, 19_000, "e")]),
        Ok(vec![dto(0, 5_000, "f")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    let stored = rig.transcripts.get_now("s1").unwrap();
    let texts: Vec<_> = stored.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c", "d", "e", "f"]);
    assert_eq!(
        stored_times(&rig),
        [
            (0, 9_000),
            (10_000, 15_000),
            (15_000, 20_000),
            (41_000, 42_000),
            (42_000, 59_000),
            (90_000, 95_000)
        ]
    );
    assert!(stored.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
}

#[test]
fn a_transient_failure_retries_only_the_piece_that_failed() {
    let spans = [span(0, 20_000), span(40_000, 60_000), span(90_000, 110_000)];
    let mut builder = pieces_rig(&spans, 1);
    builder.cloud_answers = vec![
        Ok(vec![dto(0, 100, "one")]),
        Err(AiError::Timeout),
        Ok(vec![dto(0, 100, "two")]),
        Ok(vec![dto(0, 100, "three")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(
        request_offsets(&rig.cloud),
        [0, 40_000, 40_000, 90_000],
        "only the second piece is sent twice"
    );
    assert_eq!(
        stored_times(&rig),
        [(0, 100), (40_000, 40_100), (90_000, 90_100)]
    );
}

#[test]
fn a_piece_that_keeps_failing_ends_the_track_with_a_typed_error_after_its_attempts() {
    let mut builder = pieces_rig(&[span(0, 20_000), span(40_000, 60_000)], 1);
    builder.cloud_answers = vec![
        Ok(vec![dto(0, 100, "one")]),
        Err(AiError::RateLimited { retry_after: None }),
        Err(AiError::RateLimited { retry_after: None }),
        Err(AiError::Network("reset".to_owned())),
    ];
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(
        matches!(
            error,
            TranscribeError::Engine {
                track: TrackKind::Microphone,
                error: AiError::Network(_)
            }
        ),
        "{error:?}"
    );
    assert_eq!(request_offsets(&rig.cloud), [0, 40_000, 40_000, 40_000]);
}

#[test]
fn a_permanent_failure_is_not_retried_and_the_pieces_in_flight_are_dropped() {
    let spans = [
        span(0, 20_000),
        span(40_000, 60_000),
        span(90_000, 110_000),
        span(200_000, 220_000),
    ];
    let mut builder = pieces_rig(&spans, 2);
    builder.cloud_answers = vec![Err(AiError::Auth)];
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(
        matches!(
            error,
            TranscribeError::Engine {
                error: AiError::Auth,
                ..
            }
        ),
        "{error:?}"
    );
    // Two pieces were in flight; no retry, and the later ones were never started.
    assert_eq!(request_offsets(&rig.cloud), [0, 40_000]);
}

#[test]
fn pieces_are_sent_a_few_at_a_time_never_more_than_the_limit() {
    let spans: Vec<_> = (0..9)
        .map(|i| span(i * 60_000, i * 60_000 + 20_000))
        .collect();
    let mut builder = pieces_rig(&spans, 3);
    builder.cloud_answers = (0..9)
        .map(|i| Ok(vec![dto(0, 20_000, &format!("piece {i}"))]))
        .collect();
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(rig.cloud.max_in_flight(), 3);
    let stored = rig.transcripts.get_now("s1").unwrap();
    let texts: Vec<_> = stored.iter().map(|s| s.text.clone()).collect();
    let expected: Vec<_> = (0..9).map(|i| format!("piece {i}")).collect();
    assert_eq!(texts, expected, "time order whichever piece finishes first");
}

#[test]
fn chunk_progress_counts_finished_pieces_of_each_track() {
    let spans = [span(0, 20_000), span(40_000, 60_000), span(90_000, 110_000)];
    let rig = pieces_rig(&spans, 2).build();
    let seen = Mutex::new(Vec::new());

    block_on(
        rig.subject
            .execute_with_chunk_progress(&SessionId::new("s1"), &|_| {}, &|p| {
                seen.lock().unwrap().push(p);
            }),
    )
    .unwrap();

    let chunk = |chunks_done| ChunkProgress {
        track: TrackKind::Microphone,
        chunks_done,
        chunks_total: 3,
    };
    assert_eq!(
        seen.into_inner().unwrap(),
        [chunk(0), chunk(1), chunk(2), chunk(3)]
    );
}

#[test]
fn local_whisper_never_goes_through_the_chunking_path() {
    let mut builder = RigBuilder::new();
    builder.local_answers = vec![Ok(vec![dto(0, 100, "a")]), Ok(vec![dto(0, 100, "b")])];
    let rig = builder.build();
    let chunks_seen = Mutex::new(0_usize);

    block_on(
        rig.subject
            .execute_with_chunk_progress(&SessionId::new("s1"), &|_| {}, &|_| {
                *chunks_seen.lock().unwrap() += 1;
            }),
    )
    .unwrap();

    assert_eq!(rig.speech_maps.load(Ordering::SeqCst), 0, "no speech map");
    assert_eq!(*chunks_seen.lock().unwrap(), 0);
    assert!(
        rig.cloud.requests().is_empty(),
        "the cloud engine is not used"
    );
    let requests = rig.local.requests();
    assert_eq!(requests.len(), 2, "one request per track, the whole file");
    assert!(requests
        .iter()
        .all(|r| matches!(r.audio, AudioSource::Path(_))));
}

fn request_mimes(engine: &FakeEngine) -> Vec<String> {
    engine
        .requests()
        .iter()
        .map(|request| match &request.audio {
            AudioSource::Bytes { mime_type, .. } => mime_type.clone(),
            AudioSource::Path(_) => panic!("a piece must be uploaded as bytes"),
        })
        .collect()
}

#[test]
fn a_provider_that_refuses_the_compressed_format_gets_wav_for_that_piece_and_the_rest() {
    let spans = [span(0, 20_000), span(40_000, 60_000), span(90_000, 110_000)];
    let mut builder = pieces_rig(&spans, 1);
    builder.cloud_answers = vec![
        Err(AiError::InvalidRequest(
            "unsupported file format".to_owned(),
        )),
        Ok(vec![dto(0, 100, "one")]),
        Ok(vec![dto(0, 100, "two")]),
        Ok(vec![dto(0, 100, "three")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(
        request_mimes(&rig.cloud),
        ["audio/flac", "audio/wav", "audio/wav", "audio/wav"],
        "one refusal switches the track to WAV; later pieces do not try FLAC again"
    );
    assert_eq!(request_offsets(&rig.cloud), [0, 0, 40_000, 90_000]);
    assert_eq!(
        stored_times(&rig),
        [(0, 100), (40_000, 40_100), (90_000, 90_100)]
    );
}

#[test]
fn a_refusal_of_wav_itself_is_final() {
    let mut builder = pieces_rig(&[span(0, 20_000)], 1);
    builder.cloud_answers = vec![
        Err(AiError::InvalidRequest("bad".to_owned())),
        Err(AiError::InvalidRequest("still bad".to_owned())),
    ];
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(
        matches!(
            error,
            TranscribeError::Engine {
                error: AiError::InvalidRequest(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(request_mimes(&rig.cloud), ["audio/flac", "audio/wav"]);
}

#[test]
fn a_retry_after_beyond_the_tolerated_wait_is_returned_not_retried_early() {
    let mut builder = pieces_rig(&[span(0, 20_000)], 1);
    builder.cloud_answers = vec![Err(AiError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(3_600)),
    })];
    let rig = builder.build();

    let error = block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap_err();

    assert!(
        matches!(
            error,
            TranscribeError::Engine {
                error: AiError::RateLimited { .. },
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(rig.cloud.requests().len(), 1, "no early second request");
}

#[test]
fn a_model_that_answers_200_with_an_unusable_reply_is_tried_again() {
    let mut builder = pieces_rig(&[span(0, 20_000)], 1);
    builder.cloud_answers = vec![
        Err(AiError::Provider {
            status: 200,
            message: "the transcript could not be understood".to_owned(),
        }),
        Ok(vec![dto(0, 100, "fine now")]),
    ];
    let rig = builder.build();

    block_on(rig.subject.execute(&SessionId::new("s1"))).unwrap();

    assert_eq!(rig.cloud.requests().len(), 2);
    assert_eq!(stored_times(&rig), [(0, 100)]);
}
