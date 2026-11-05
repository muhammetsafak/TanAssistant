//! Ports: the traits adapters implement.

pub mod repositories;
pub mod secrets;

use tan_domain::EpochMs;

pub use repositories::{
    FrameRepository, NoteRepository, ProviderProfileRepository, SearchHit, SearchIndex,
    SearchSource, SecretMetadataRepository, SessionRepository, SettingsRepository,
    TaskAssignmentRepository, TranscriptRepository, VariableRepository,
};
pub use secrets::{
    SecretBackend, SecretStore, SecretStoreControl, SecretStoreError, SecretStoreResult,
    SecretStoreStatus, SecretValue,
};

/// Mints identifiers for new entities (the domain stays free of randomness).
pub trait IdGenerator: Send + Sync {
    /// A new unique id; adapters use time-ordered UUID v7.
    fn new_id(&self) -> String;
}

/// The current time (the domain stays free of clocks).
pub trait Clock: Send + Sync {
    /// Now, in milliseconds since the Unix epoch.
    fn now(&self) -> EpochMs;
}
