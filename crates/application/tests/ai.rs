//! AI use cases (profiles, assignments, connection test) against in-memory fakes.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use tan_application::ai::{
    AiUseCaseError, ClearTaskAssignment, DeleteProviderProfile, GetTaskAssignments,
    ListProviderProfiles, SetTaskAssignment, TestProvider, UpsertProviderProfile,
    UpsertProviderProfileInput,
};
use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, Clock, IdGenerator, ModelCatalog,
    ProviderFactory, ProviderProfileRepository, TaskAssignmentRepository, TranscriptSegmentDto,
    TranscriptionModel, TranscriptionRequest, VisionModel, VisionRequest,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    EpochMs, ProviderKind, ProviderProfile, ProviderProfileId, SecretId, TaskAssignment, TaskKind,
};

// ---- fakes -----------------------------------------------------------------------------

#[derive(Default)]
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    fn new_id(&self) -> String {
        format!("profile-{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

/// Every `now()` call advances by 25 ms, so a test sees a deterministic latency.
#[derive(Default)]
struct SteppingClock(AtomicI64);

impl Clock for SteppingClock {
    fn now(&self) -> EpochMs {
        EpochMs::new(self.0.fetch_add(25, Ordering::SeqCst))
    }
}

/// Profiles plus the assignments that cascade when a profile goes away, like the SQLite
/// repositories do.
struct MemStore {
    profiles: Mutex<BTreeMap<String, ProviderProfile>>,
    assignments: Mutex<BTreeMap<&'static str, TaskAssignment>>,
    existing_secrets: BTreeSet<String>,
}

impl MemStore {
    fn new(secrets: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            profiles: Mutex::default(),
            assignments: Mutex::default(),
            existing_secrets: secrets.iter().map(|s| (*s).to_owned()).collect(),
        })
    }
}

impl ProviderProfileRepository for MemStore {
    async fn upsert(&self, profile: &ProviderProfile) -> RepositoryResult<()> {
        if let Some(secret) = &profile.secret_ref {
            if !self.existing_secrets.contains(secret.as_str()) {
                return Err(RepositoryError::Conflict("dangling secret".to_owned()));
            }
        }
        self.profiles
            .lock()
            .unwrap()
            .insert(profile.id.to_string(), profile.clone());
        Ok(())
    }

    async fn get(&self, id: &ProviderProfileId) -> RepositoryResult<Option<ProviderProfile>> {
        Ok(self.profiles.lock().unwrap().get(id.as_str()).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<ProviderProfile>> {
        let mut all: Vec<_> = self.profiles.lock().unwrap().values().cloned().collect();
        all.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        Ok(all)
    }

    async fn delete(&self, id: &ProviderProfileId) -> RepositoryResult<()> {
        self.profiles
            .lock()
            .unwrap()
            .remove(id.as_str())
            .ok_or_else(|| RepositoryError::not_found("provider profile", id.as_str()))?;
        self.assignments
            .lock()
            .unwrap()
            .retain(|_, a| &a.profile_id != id);
        Ok(())
    }
}

impl TaskAssignmentRepository for MemStore {
    async fn set(&self, assignment: &TaskAssignment) -> RepositoryResult<()> {
        if !self
            .profiles
            .lock()
            .unwrap()
            .contains_key(assignment.profile_id.as_str())
        {
            return Err(RepositoryError::Conflict("dangling profile".to_owned()));
        }
        self.assignments
            .lock()
            .unwrap()
            .insert(assignment.task.as_str(), assignment.clone());
        Ok(())
    }

    async fn get(&self, task: TaskKind) -> RepositoryResult<Option<TaskAssignment>> {
        Ok(self.assignments.lock().unwrap().get(task.as_str()).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<TaskAssignment>> {
        Ok(self.assignments.lock().unwrap().values().cloned().collect())
    }

    async fn clear(&self, task: TaskKind) -> RepositoryResult<()> {
        self.assignments.lock().unwrap().remove(task.as_str());
        Ok(())
    }
}

/// One fake adapter for every capability; it answers from a script.
#[derive(Clone)]
struct FakeModel {
    reply: Result<String, AiError>,
    models: Result<Vec<String>, AiError>,
}

impl ChatModel for FakeModel {
    async fn complete(&self, _request: ChatRequest) -> AiResult<ChatResponse> {
        self.reply.clone().map(|text| ChatResponse {
            text,
            usage: None,
            truncated: false,
        })
    }
}

impl VisionModel for FakeModel {
    async fn describe(&self, _request: VisionRequest) -> AiResult<ChatResponse> {
        Err(AiError::Unsupported("not scripted".to_owned()))
    }
}

impl TranscriptionModel for FakeModel {
    async fn transcribe(
        &self,
        _request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        Err(AiError::Unsupported("not scripted".to_owned()))
    }
}

impl ModelCatalog for FakeModel {
    async fn list_models(&self) -> AiResult<Vec<String>> {
        self.models.clone()
    }
}

struct FakeFactory(FakeModel);

impl ProviderFactory for FakeFactory {
    type Chat = FakeModel;
    type Vision = FakeModel;
    type Transcription = FakeModel;
    type Catalog = FakeModel;

    async fn chat(&self, _profile: &ProviderProfile) -> AiResult<FakeModel> {
        Ok(self.0.clone())
    }

    async fn vision(&self, _profile: &ProviderProfile) -> AiResult<FakeModel> {
        Ok(self.0.clone())
    }

    async fn transcription(&self, _profile: &ProviderProfile) -> AiResult<FakeModel> {
        Ok(self.0.clone())
    }

    async fn catalog(&self, _profile: &ProviderProfile) -> AiResult<FakeModel> {
        Ok(self.0.clone())
    }
}

// ---- helpers ---------------------------------------------------------------------------

type Upsert = UpsertProviderProfile<MemStore, MemStore, SequentialIds>;

fn upsert_use_case(store: &Arc<MemStore>) -> Upsert {
    UpsertProviderProfile::new(
        store.clone(),
        store.clone(),
        Arc::new(SequentialIds::default()),
    )
}

fn input(kind: ProviderKind) -> UpsertProviderProfileInput {
    UpsertProviderProfileInput {
        id: None,
        kind,
        display_name: "  My profile ".to_owned(),
        model: " some-model ".to_owned(),
        base_url: None,
        secret_ref: Some(SecretId::new("key-1")),
        supports_vision: false,
    }
}

fn create(store: &Arc<MemStore>, kind: ProviderKind) -> ProviderProfile {
    let mut request = input(kind);
    if kind == ProviderKind::Ollama {
        request.secret_ref = None;
    }
    block_on(upsert_use_case(store).execute(request)).unwrap()
}

fn assign(
    store: &Arc<MemStore>,
    task: TaskKind,
    profile: &ProviderProfile,
) -> Result<TaskAssignment, AiUseCaseError> {
    block_on(SetTaskAssignment::new(store.clone(), store.clone()).execute(task, &profile.id))
}

// ---- assignments -----------------------------------------------------------------------

#[test]
fn deepseek_cannot_be_assigned_vision_or_transcription() {
    let store = MemStore::new(&["key-1"]);
    let deepseek = create(&store, ProviderKind::DeepSeek);

    for task in [TaskKind::Vision, TaskKind::Transcription] {
        let err = assign(&store, task, &deepseek).unwrap_err();
        assert!(
            matches!(
                err,
                AiUseCaseError::CapabilityMissing { task: t, provider: ProviderKind::DeepSeek } if t == task
            ),
            "{err:?}"
        );
    }
    assert!(
        block_on(TaskAssignmentRepository::list(&*store))
            .unwrap()
            .is_empty(),
        "nothing was stored"
    );
    // Chat is its one capability.
    assert!(assign(&store, TaskKind::Summary, &deepseek).is_ok());
}

#[test]
fn anthropic_cannot_be_assigned_transcription() {
    let store = MemStore::new(&["key-1"]);
    let anthropic = create(&store, ProviderKind::Anthropic);
    let err = assign(&store, TaskKind::Transcription, &anthropic).unwrap_err();
    assert!(matches!(
        err,
        AiUseCaseError::CapabilityMissing {
            task: TaskKind::Transcription,
            provider: ProviderKind::Anthropic
        }
    ));
    assert!(err.to_string().contains("anthropic"));
    assert!(assign(&store, TaskKind::Vision, &anthropic).is_ok());
    assert!(assign(&store, TaskKind::Summary, &anthropic).is_ok());
}

fn ollama_input(supports_vision: bool) -> UpsertProviderProfileInput {
    UpsertProviderProfileInput {
        secret_ref: None,
        supports_vision,
        ..input(ProviderKind::Ollama)
    }
}

#[test]
fn ollama_vision_needs_the_profile_flag() {
    let store = MemStore::new(&[]);

    // Default (flag off): chat only.
    let text_only = block_on(upsert_use_case(&store).execute(ollama_input(false))).unwrap();
    assert!(!text_only.supports_vision);
    assert!(assign(&store, TaskKind::Summary, &text_only).is_ok());
    let err = assign(&store, TaskKind::Vision, &text_only).unwrap_err();
    assert!(matches!(
        err,
        AiUseCaseError::CapabilityMissing {
            task: TaskKind::Vision,
            provider: ProviderKind::Ollama
        }
    ));

    // Flag on: vision is accepted, transcription still is not.
    let with_vision = block_on(upsert_use_case(&store).execute(ollama_input(true))).unwrap();
    assert!(with_vision.supports_vision);
    assert!(assign(&store, TaskKind::Vision, &with_vision).is_ok());
    assert!(matches!(
        assign(&store, TaskKind::Transcription, &with_vision),
        Err(AiUseCaseError::CapabilityMissing { .. })
    ));
}

#[test]
fn switching_the_ollama_vision_flag_off_is_refused_while_it_is_assigned() {
    let store = MemStore::new(&[]);
    let profile = block_on(upsert_use_case(&store).execute(ollama_input(true))).unwrap();
    assign(&store, TaskKind::Vision, &profile).unwrap();

    let mut off = ollama_input(false);
    off.id = Some(profile.id.clone());
    assert!(matches!(
        block_on(upsert_use_case(&store).execute(off)),
        Err(AiUseCaseError::CapabilityMissing {
            task: TaskKind::Vision,
            ..
        })
    ));
    let stored = block_on(ProviderProfileRepository::get(&*store, &profile.id))
        .unwrap()
        .unwrap();
    assert!(stored.supports_vision, "the profile was left alone");

    block_on(ClearTaskAssignment::new(store.clone()).execute(TaskKind::Vision)).unwrap();
    let mut off = ollama_input(false);
    off.id = Some(profile.id);
    assert!(
        !block_on(upsert_use_case(&store).execute(off))
            .unwrap()
            .supports_vision
    );
}

#[test]
fn the_vision_flag_only_matters_for_ollama() {
    let store = MemStore::new(&["key-1"]);
    // Claiming vision for DeepSeek does not give it vision; OpenAI keeps it regardless.
    let deepseek = block_on(upsert_use_case(&store).execute(UpsertProviderProfileInput {
        supports_vision: true,
        ..input(ProviderKind::DeepSeek)
    }))
    .unwrap();
    assert!(!deepseek.supports_vision);
    assert!(matches!(
        assign(&store, TaskKind::Vision, &deepseek),
        Err(AiUseCaseError::CapabilityMissing { .. })
    ));
    let openai = block_on(upsert_use_case(&store).execute(input(ProviderKind::OpenAi))).unwrap();
    assert!(openai.supports_vision);
    assert!(assign(&store, TaskKind::Vision, &openai).is_ok());
}

#[test]
fn openai_and_gemini_take_every_task() {
    let store = MemStore::new(&["key-1"]);
    for kind in [ProviderKind::OpenAi, ProviderKind::Gemini] {
        let profile = create(&store, kind);
        for &task in TaskKind::ALL {
            assert!(assign(&store, task, &profile).is_ok(), "{kind} {task}");
        }
    }
}

#[test]
fn assigning_an_unknown_profile_is_not_found() {
    let store = MemStore::new(&[]);
    let err = block_on(
        SetTaskAssignment::new(store.clone(), store.clone())
            .execute(TaskKind::Summary, &ProviderProfileId::new("nope")),
    )
    .unwrap_err();
    assert!(matches!(err, AiUseCaseError::NotFound { .. }));
}

#[test]
fn assignments_can_be_read_replaced_and_cleared() {
    let store = MemStore::new(&["key-1"]);
    let first = create(&store, ProviderKind::OpenAi);
    let second = create(&store, ProviderKind::Gemini);
    assign(&store, TaskKind::Transcription, &first).unwrap();
    assign(&store, TaskKind::Transcription, &second).unwrap();
    assign(&store, TaskKind::Summary, &first).unwrap();

    let all = block_on(GetTaskAssignments::new(store.clone()).execute()).unwrap();
    assert_eq!(all.len(), 2);
    let transcription = all
        .iter()
        .find(|a| a.task == TaskKind::Transcription)
        .unwrap();
    assert_eq!(transcription.profile_id, second.id);

    let clear = ClearTaskAssignment::new(store.clone());
    block_on(clear.execute(TaskKind::Transcription)).unwrap();
    block_on(clear.execute(TaskKind::Transcription)).unwrap();
    assert_eq!(
        block_on(GetTaskAssignments::new(store.clone()).execute())
            .unwrap()
            .len(),
        1
    );
}

// ---- profiles --------------------------------------------------------------------------

#[test]
fn upsert_creates_with_a_new_id_and_trims_text() {
    let store = MemStore::new(&["key-1"]);
    let profile = block_on(upsert_use_case(&store).execute(input(ProviderKind::OpenAi))).unwrap();
    assert_eq!(profile.id.as_str(), "profile-0");
    assert_eq!(profile.display_name, "My profile");
    assert_eq!(profile.model, "some-model");
    assert_eq!(profile.base_url, None);

    let listed = block_on(ListProviderProfiles::new(store.clone()).execute()).unwrap();
    assert_eq!(listed, vec![profile]);
}

#[test]
fn upsert_updates_an_existing_profile_by_id() {
    let store = MemStore::new(&["key-1"]);
    let created = create(&store, ProviderKind::OpenAi);
    let mut change = input(ProviderKind::OpenAi);
    change.id = Some(created.id.clone());
    change.model = "other-model".to_owned();
    change.base_url = Some(" https://proxy.example/v1/ ".to_owned());
    let updated = block_on(upsert_use_case(&store).execute(change)).unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.model, "other-model");
    assert_eq!(
        updated.base_url.as_deref(),
        Some("https://proxy.example/v1")
    );
    assert_eq!(block_on(store.list_profiles()).len(), 1);
}

impl MemStore {
    async fn list_profiles(&self) -> Vec<ProviderProfile> {
        ProviderProfileRepository::list(self).await.unwrap()
    }
}

#[test]
fn upsert_of_an_unknown_id_is_not_found() {
    let store = MemStore::new(&["key-1"]);
    let mut request = input(ProviderKind::OpenAi);
    request.id = Some(ProviderProfileId::new("ghost"));
    assert!(matches!(
        block_on(upsert_use_case(&store).execute(request)),
        Err(AiUseCaseError::NotFound { .. })
    ));
}

#[test]
fn upsert_validates_its_input() {
    let store = MemStore::new(&["key-1"]);
    let run = |mutate: &dyn Fn(&mut UpsertProviderProfileInput)| {
        let mut request = input(ProviderKind::OpenAi);
        mutate(&mut request);
        block_on(upsert_use_case(&store).execute(request))
    };

    assert!(matches!(
        run(&|r| r.display_name = "  ".to_owned()),
        Err(AiUseCaseError::Invalid(_))
    ));
    assert!(matches!(
        run(&|r| r.model = String::new()),
        Err(AiUseCaseError::Invalid(_))
    ));
    assert!(matches!(
        run(&|r| r.model = "x".repeat(201)),
        Err(AiUseCaseError::Invalid(_))
    ));
    assert!(matches!(
        run(&|r| r.model = "bad\u{0}model".to_owned()),
        Err(AiUseCaseError::Invalid(_))
    ));
    // A keyed provider needs a key.
    assert!(matches!(
        run(&|r| r.secret_ref = None),
        Err(AiUseCaseError::Invalid(_))
    ));
    // Keys do not travel over plain http (except to localhost).
    assert!(matches!(
        run(&|r| r.base_url = Some("http://proxy.example/v1".to_owned())),
        Err(AiUseCaseError::Invalid(_))
    ));
    assert!(run(&|r| r.base_url = Some("http://localhost:8080/v1".to_owned())).is_ok());
    assert!(matches!(
        run(&|r| r.base_url = Some("https://user:pw@proxy.example".to_owned())),
        Err(AiUseCaseError::Invalid(_))
    ));
    // A reference to a secret that does not exist is a conflict.
    assert!(matches!(
        run(&|r| r.secret_ref = Some(SecretId::new("ghost"))),
        Err(AiUseCaseError::Conflict(_))
    ));
}

#[test]
fn an_ollama_profile_with_a_key_must_use_https_for_remote_hosts() {
    let store = MemStore::new(&["key-1"]);
    let keyed = |url: &str| UpsertProviderProfileInput {
        secret_ref: Some(SecretId::new("key-1")),
        base_url: Some(url.to_owned()),
        ..ollama_input(false)
    };
    assert!(matches!(
        block_on(upsert_use_case(&store).execute(keyed("http://ollama.example.com:11434"))),
        Err(AiUseCaseError::Invalid(_))
    ));
    assert!(block_on(upsert_use_case(&store).execute(keyed("https://ollama.example.com"))).is_ok());
    assert!(block_on(upsert_use_case(&store).execute(keyed("http://localhost:11434"))).is_ok());
}

#[test]
fn cli_profiles_take_neither_a_secret_nor_a_base_url() {
    let store = MemStore::new(&["key-1"]);
    for kind in [ProviderKind::ClaudeCli, ProviderKind::CodexCli] {
        // `input` carries a secret reference.
        let mut with_secret = input(kind);
        with_secret.secret_ref = Some(SecretId::new("key-1"));
        assert!(matches!(
            block_on(upsert_use_case(&store).execute(with_secret)),
            Err(AiUseCaseError::Invalid(_))
        ));

        let mut with_url = input(kind);
        with_url.secret_ref = None;
        with_url.base_url = Some("https://proxy.example.com".to_owned());
        assert!(matches!(
            block_on(upsert_use_case(&store).execute(with_url)),
            Err(AiUseCaseError::Invalid(_))
        ));

        // A blank URL counts as none; the profile is stored and can serve chat and vision.
        let mut plain = input(kind);
        plain.secret_ref = None;
        plain.base_url = Some("  ".to_owned());
        plain.supports_vision = false;
        let profile = block_on(upsert_use_case(&store).execute(plain)).unwrap();
        assert_eq!(profile.base_url, None);
        assert_eq!(profile.secret_ref, None);
        assert!(profile.supports_vision, "the matrix decides, not the input");
        assign(&store, TaskKind::Summary, &profile).unwrap();
        assign(&store, TaskKind::Vision, &profile).unwrap();
        assert!(matches!(
            assign(&store, TaskKind::Transcription, &profile),
            Err(AiUseCaseError::CapabilityMissing { .. })
        ));
    }
}

#[test]
fn ollama_needs_no_key_and_may_use_plain_http() {
    let store = MemStore::new(&[]);
    let mut request = input(ProviderKind::Ollama);
    request.secret_ref = None;
    request.base_url = Some("http://192.168.1.20:11434".to_owned());
    let profile = block_on(upsert_use_case(&store).execute(request)).unwrap();
    assert_eq!(
        profile.base_url.as_deref(),
        Some("http://192.168.1.20:11434")
    );
}

#[test]
fn changing_the_kind_is_refused_when_an_assignment_would_break() {
    let store = MemStore::new(&["key-1"]);
    let openai = create(&store, ProviderKind::OpenAi);
    assign(&store, TaskKind::Vision, &openai).unwrap();

    let mut change = input(ProviderKind::DeepSeek);
    change.id = Some(openai.id.clone());
    let err = block_on(upsert_use_case(&store).execute(change)).unwrap_err();
    assert!(matches!(
        err,
        AiUseCaseError::CapabilityMissing {
            task: TaskKind::Vision,
            provider: ProviderKind::DeepSeek
        }
    ));
    let stored = block_on(ProviderProfileRepository::get(&*store, &openai.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.kind,
        ProviderKind::OpenAi,
        "the profile was left alone"
    );

    // Without the assignment the switch is fine.
    block_on(ClearTaskAssignment::new(store.clone()).execute(TaskKind::Vision)).unwrap();
    let mut change = input(ProviderKind::DeepSeek);
    change.id = Some(openai.id);
    assert!(block_on(upsert_use_case(&store).execute(change)).is_ok());
}

#[test]
fn deleting_a_profile_drops_its_assignments() {
    let store = MemStore::new(&["key-1"]);
    let profile = create(&store, ProviderKind::OpenAi);
    assign(&store, TaskKind::Summary, &profile).unwrap();
    let delete = DeleteProviderProfile::new(store.clone());
    block_on(delete.execute(&profile.id)).unwrap();
    assert_eq!(
        block_on(TaskAssignmentRepository::list(&*store))
            .unwrap()
            .len(),
        0
    );
    assert!(matches!(
        block_on(delete.execute(&profile.id)),
        Err(AiUseCaseError::NotFound { .. })
    ));
}

// ---- connection test -------------------------------------------------------------------

fn tester(
    store: &Arc<MemStore>,
    model: FakeModel,
) -> TestProvider<MemStore, FakeFactory, SteppingClock> {
    TestProvider::new(
        store.clone(),
        Arc::new(FakeFactory(model)),
        Arc::new(SteppingClock::default()),
    )
}

fn scripted(reply: Result<&str, AiError>, models: Result<Vec<&str>, AiError>) -> FakeModel {
    FakeModel {
        reply: reply.map(str::to_owned),
        models: models.map(|m| m.into_iter().map(str::to_owned).collect()),
    }
}

#[test]
fn test_provider_runs_a_chat_call_and_reports_latency() {
    let store = MemStore::new(&["key-1"]);
    let profile = create(&store, ProviderKind::OpenAi);
    let long_reply = "pong ".repeat(40);
    let report =
        block_on(tester(&store, scripted(Ok(&long_reply), Ok(vec![]))).execute(&profile.id))
            .unwrap();
    assert_eq!(report.latency_ms, 25);
    assert_eq!(report.model_found, None);
    assert_eq!(report.available_models.len(), 0);
    assert_eq!(report.reply_preview.unwrap().chars().count(), 80);
}

#[test]
fn test_provider_lists_models_for_ollama_and_checks_the_profile_model() {
    let store = MemStore::new(&[]);
    let mut request = input(ProviderKind::Ollama);
    request.secret_ref = None;
    request.model = "llama3.2".to_owned();
    let profile = block_on(upsert_use_case(&store).execute(request)).unwrap();

    let found = block_on(
        tester(
            &store,
            scripted(
                Err(AiError::Timeout),
                Ok(vec!["llama3.2:latest", "llava:7b"]),
            ),
        )
        .execute(&profile.id),
    )
    .unwrap();
    assert_eq!(found.model_found, Some(true));
    assert_eq!(found.available_models.len(), 2);
    assert_eq!(found.reply_preview, None, "no chat call is made for Ollama");

    let missing = block_on(
        tester(
            &store,
            scripted(Err(AiError::Timeout), Ok(vec!["llava:7b"])),
        )
        .execute(&profile.id),
    )
    .unwrap();
    assert_eq!(missing.model_found, Some(false));
}

#[test]
fn test_provider_surfaces_provider_failures_and_unknown_profiles() {
    let store = MemStore::new(&["key-1"]);
    let profile = create(&store, ProviderKind::OpenAi);
    let err =
        block_on(tester(&store, scripted(Err(AiError::Auth), Ok(vec![]))).execute(&profile.id))
            .unwrap_err();
    assert!(matches!(err, AiUseCaseError::Provider(AiError::Auth)));

    let err = block_on(
        tester(&store, scripted(Ok("pong"), Ok(vec![]))).execute(&ProviderProfileId::new("ghost")),
    )
    .unwrap_err();
    assert!(matches!(err, AiUseCaseError::NotFound { .. }));
}
