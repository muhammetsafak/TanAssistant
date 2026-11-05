//! Ports: the traits adapters implement.

pub mod repositories;

pub use repositories::{
    FrameRepository, NoteRepository, ProviderProfileRepository, SearchHit, SearchIndex,
    SearchSource, SecretMetadataRepository, SessionRepository, SettingsRepository,
    TaskAssignmentRepository, TranscriptRepository, VariableRepository,
};
