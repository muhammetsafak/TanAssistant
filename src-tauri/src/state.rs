//! Composition root state: builds the adapters and wires them into the use cases.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tan_application::ai::{
    ClearTaskAssignment, DeleteProviderProfile, GetTaskAssignments, ListProviderProfiles,
    SetTaskAssignment, TestProvider, UpsertProviderProfile,
};
use tan_application::credentials::{
    CreateSecret, DeleteSecret, DeleteVariable, GetSecretStoreStatus, ListSecrets, ListVariables,
    ResolveSecret, RevealSecret, SetMasterPassword, UnlockSecretStore, UpdateSecret,
    UpsertVariable,
};
use tan_application::ports::{Clock, IdGenerator};
use tan_application::recording::{RecordingEvents, RecordingPorts, RecordingService};
use tan_application::settings::{GetSettings, UpdateSettings};
use tan_application::transcription::{TranscribePorts, TranscribeSession};
use tan_domain::EpochMs;
use tan_infra_ai::AiProviderFactory;
use tan_infra_capture::{DHasher, FileMediaStore, PlatformAudioCapture, PlatformScreenCapture};
use tan_infra_secrets::{open_secret_store, SelectedSecretStore};
use tan_infra_storage::{
    new_id, DataLayout, Database, SqliteFrameRepository, SqliteProviderProfileRepository,
    SqliteSecretMetadataRepository, SqliteSessionRepository, SqliteSettingsRepository,
    SqliteTaskAssignmentRepository, SqliteTranscriptRepository, SqliteVariableRepository,
    StorageError,
};
use tan_infra_transcription::{
    ModelError, ModelManager, SessionFiles, WavAudioChunker, WhisperTranscriber,
};

/// Environment variable that overrides the data directory (development, tests, portable use).
pub const DATA_DIR_ENV: &str = "TAN_DATA_DIR";

/// Mints UUID v7 identifiers.
#[derive(Debug, Default)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn new_id(&self) -> String {
        new_id()
    }
}

/// Reads the system clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> EpochMs {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        EpochMs::new(millis)
    }
}

/// Why the application state could not be built.
#[derive(Debug)]
pub enum StartupError {
    /// The OS reported no home directory to place the data directory in.
    NoDataDirectory,
    /// The data directory could not be created.
    DataDirectory(std::io::Error),
    /// The database could not be opened or migrated.
    Database(StorageError),
    /// The secret store could not be selected.
    SecretStore(tan_application::ports::SecretStoreError),
    /// The HTTP client for the AI providers could not be created.
    AiClient(tan_application::ports::AiError),
    /// The whisper model manager could not be created.
    Models(ModelError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDataDirectory => write!(
                f,
                "no data directory could be determined; set {DATA_DIR_ENV} to choose one"
            ),
            Self::DataDirectory(e) => write!(f, "cannot create the data directory: {e}"),
            Self::Database(e) => write!(f, "cannot open the database: {e}"),
            Self::SecretStore(e) => write!(f, "cannot set up the secret store: {e}"),
            Self::AiClient(e) => write!(f, "cannot set up the AI client: {e}"),
            Self::Models(e) => write!(f, "cannot set up the whisper models: {e}"),
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoDataDirectory => None,
            Self::DataDirectory(e) => Some(e),
            Self::Database(e) => Some(e),
            Self::SecretStore(e) => Some(e),
            Self::AiClient(e) => Some(e),
            Self::Models(e) => Some(e),
        }
    }
}

type Metadata = SqliteSecretMetadataRepository;
type Variables = SqliteVariableRepository;
type Profiles = SqliteProviderProfileRepository;
type Assignments = SqliteTaskAssignmentRepository;
type AiFactory = AiProviderFactory<Metadata, SelectedSecretStore>;
/// The recording use case wired to the SQLite repositories.
pub type Recording =
    RecordingService<SqliteSessionRepository, SqliteFrameRepository, SqliteSettingsRepository>;

/// The transcription use case wired to SQLite, local whisper and the AI providers.
pub type Transcribe = TranscribeSession<
    SqliteSessionRepository,
    SqliteTranscriptRepository,
    Assignments,
    Profiles,
    AiFactory,
    WhisperTranscriber,
    WavAudioChunker,
    SessionFiles,
>;

/// Everything the IPC commands need, built once at startup and managed by Tauri.
pub struct AppState {
    /// Where application data lives.
    pub layout: DataLayout,
    /// Lists variables.
    pub list_variables: ListVariables<Variables>,
    /// Creates or updates a variable.
    pub upsert_variable: UpsertVariable<Variables, UuidGenerator>,
    /// Deletes a variable.
    pub delete_variable: DeleteVariable<Variables>,
    /// Lists secret metadata.
    pub list_secrets: ListSecrets<Metadata>,
    /// Creates a secret.
    pub create_secret: CreateSecret<Metadata, SelectedSecretStore, UuidGenerator, SystemClock>,
    /// Updates a secret.
    pub update_secret: UpdateSecret<Metadata, SelectedSecretStore>,
    /// Deletes a secret.
    pub delete_secret: DeleteSecret<Metadata, SelectedSecretStore>,
    /// Reveals a secret's value on the user's explicit request (exposed over IPC).
    pub reveal_secret: RevealSecret<Metadata, SelectedSecretStore>,
    /// Reads a secret's value for other services. Deliberately not exposed over IPC.
    pub resolve_secret: Arc<ResolveSecret<Metadata, SelectedSecretStore>>,
    /// Reports the secret store's state.
    pub secret_store_status: GetSecretStoreStatus<SelectedSecretStore>,
    /// Unlocks the encrypted-file store (slow: run on a blocking thread).
    pub unlock_secret_store: Arc<UnlockSecretStore<SelectedSecretStore>>,
    /// Sets or changes the master password (slow: run on a blocking thread).
    pub set_master_password: Arc<SetMasterPassword<SelectedSecretStore>>,
    /// Reads the settings.
    pub get_settings: GetSettings<SqliteSettingsRepository>,
    /// Updates the settings.
    pub update_settings: UpdateSettings<SqliteSettingsRepository>,
    /// Lists AI provider profiles.
    pub list_provider_profiles: ListProviderProfiles<Profiles>,
    /// Creates or updates an AI provider profile.
    pub upsert_provider_profile: UpsertProviderProfile<Profiles, Assignments, UuidGenerator>,
    /// Deletes an AI provider profile.
    pub delete_provider_profile: DeleteProviderProfile<Profiles>,
    /// Tests an AI provider profile with a short call.
    pub test_provider: TestProvider<Profiles, AiFactory, SystemClock>,
    /// Assigns a task to a provider profile.
    pub set_task_assignment: SetTaskAssignment<Profiles, Assignments>,
    /// Reads the task assignments.
    pub get_task_assignments: GetTaskAssignments<Assignments>,
    /// Clears a task assignment.
    pub clear_task_assignment: ClearTaskAssignment<Assignments>,
    /// Builds chat/vision/transcription adapters from profiles. For the processing pipeline;
    /// deliberately not exposed over IPC.
    pub ai_factory: Arc<AiFactory>,
    /// Records meetings: microphone, system audio and changed screen frames.
    pub recording: Arc<Recording>,
    /// Lists microphones for the recording options.
    pub audio_capture: Arc<PlatformAudioCapture>,
    /// Lists displays for the recording options.
    pub screen_capture: Arc<PlatformScreenCapture>,
    /// The whisper model files (`<data>/models/`).
    pub whisper_models: Arc<ModelManager>,
    /// Transcribes a session: the assigned provider, or local whisper. The processing pipeline
    /// calls it too.
    pub transcribe_session: Transcribe,
}

/// The data directory: `TAN_DATA_DIR` if set, otherwise the OS application data directory.
pub fn resolve_layout() -> Result<DataLayout, StartupError> {
    match std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        Some(dir) => Ok(DataLayout::new(dir)),
        None => DataLayout::for_current_os().ok_or(StartupError::NoDataDirectory),
    }
}

impl AppState {
    /// Opens the database and the secret store under `layout` and wires every use case.
    /// `recording_events` receives the progress and state of recordings (the UI shell).
    pub fn build(
        layout: DataLayout,
        recording_events: Arc<dyn RecordingEvents>,
    ) -> Result<Self, StartupError> {
        layout.ensure_root().map_err(StartupError::DataDirectory)?;
        let database = Database::open(&layout.database_path()).map_err(StartupError::Database)?;
        let secret_store =
            Arc::new(open_secret_store(layout.root()).map_err(StartupError::SecretStore)?);
        let ids = Arc::new(UuidGenerator);
        let clock = Arc::new(SystemClock);

        let variables = Arc::new(SqliteVariableRepository::new(database.clone()));
        let metadata = Arc::new(SqliteSecretMetadataRepository::new(database.clone()));
        let profiles = Arc::new(SqliteProviderProfileRepository::new(database.clone()));
        let assignments = Arc::new(SqliteTaskAssignmentRepository::new(database.clone()));
        let sessions = Arc::new(SqliteSessionRepository::new(database.clone()));
        let frames = Arc::new(SqliteFrameRepository::new(database.clone()));
        let transcripts = Arc::new(SqliteTranscriptRepository::new(database.clone()));
        let settings = Arc::new(SqliteSettingsRepository::new(database));
        let resolve_secret = Arc::new(ResolveSecret::new(metadata.clone(), secret_store.clone()));
        let ai_factory = Arc::new(
            AiProviderFactory::new(resolve_secret.clone()).map_err(StartupError::AiClient)?,
        );

        let audio_capture = Arc::new(PlatformAudioCapture);
        let screen_capture = Arc::new(PlatformScreenCapture);
        let whisper_models = Arc::new(
            ModelManager::new(layout.root().join("models")).map_err(StartupError::Models)?,
        );
        let transcribe_session = TranscribeSession::new(TranscribePorts {
            sessions: sessions.clone(),
            transcripts,
            assignments: assignments.clone(),
            profiles: profiles.clone(),
            factory: ai_factory.clone(),
            local: Arc::new(WhisperTranscriber::new(whisper_models.clone())),
            chunker: Arc::new(WavAudioChunker),
            files: Arc::new(SessionFiles::new(layout.sessions_dir())),
        });
        let recording = Arc::new(RecordingService::new(
            sessions,
            frames,
            settings.clone(),
            RecordingPorts {
                audio: audio_capture.clone(),
                screen: screen_capture.clone(),
                hasher: Arc::new(DHasher),
                media: Arc::new(FileMediaStore::new(layout.sessions_dir())),
                ids: ids.clone(),
                clock: clock.clone(),
                events: recording_events,
            },
        ));

        Ok(Self {
            layout,
            list_variables: ListVariables::new(variables.clone()),
            upsert_variable: UpsertVariable::new(variables.clone(), ids.clone()),
            delete_variable: DeleteVariable::new(variables),
            list_secrets: ListSecrets::new(metadata.clone()),
            create_secret: CreateSecret::new(
                metadata.clone(),
                secret_store.clone(),
                ids.clone(),
                clock.clone(),
            ),
            update_secret: UpdateSecret::new(metadata.clone(), secret_store.clone()),
            delete_secret: DeleteSecret::new(metadata.clone(), secret_store.clone()),
            reveal_secret: RevealSecret::new(metadata.clone(), secret_store.clone()),
            resolve_secret,
            secret_store_status: GetSecretStoreStatus::new(secret_store.clone()),
            unlock_secret_store: Arc::new(UnlockSecretStore::new(secret_store.clone())),
            set_master_password: Arc::new(SetMasterPassword::new(secret_store)),
            get_settings: GetSettings::new(settings.clone()),
            update_settings: UpdateSettings::new(settings),
            list_provider_profiles: ListProviderProfiles::new(profiles.clone()),
            upsert_provider_profile: UpsertProviderProfile::new(
                profiles.clone(),
                assignments.clone(),
                ids,
            ),
            delete_provider_profile: DeleteProviderProfile::new(profiles.clone()),
            test_provider: TestProvider::new(profiles.clone(), ai_factory.clone(), clock),
            set_task_assignment: SetTaskAssignment::new(profiles, assignments.clone()),
            get_task_assignments: GetTaskAssignments::new(assignments.clone()),
            clear_task_assignment: ClearTaskAssignment::new(assignments),
            ai_factory,
            recording,
            audio_capture,
            screen_capture,
            whisper_models,
            transcribe_session,
        })
    }
}
