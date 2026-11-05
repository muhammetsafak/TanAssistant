//! Repository ports.
//!
//! Methods are written as `fn ... -> impl Future<Output = ...> + Send` (what `async fn` in a
//! trait desugars to, plus the `Send` bound a multi-threaded executor needs). Implementors
//! can still write plain `async fn`. No async runtime is required by this crate.
//!
//! Conventions: `get*` returns `Ok(None)` for a missing entity; `update*`/`delete*` return
//! [`RepositoryError::NotFound`] when the target does not exist; `create*` returns
//! [`RepositoryError::Conflict`] on duplicates.

use std::future::Future;

use tan_domain::{
    ActionItem, ActionItemId, AppSettings, AudioTrack, Frame, FrameId, Note, NoteId,
    ProviderProfile, ProviderProfileId, SecretId, SecretRef, Session, SessionId, SettingsPatch,
    TaskAssignment, TaskKind, TranscriptSegment, Variable, VariableId,
};

use crate::error::RepositoryResult;

/// Persistence of sessions and their audio tracks.
pub trait SessionRepository: Send + Sync {
    /// Inserts a new session.
    fn create(&self, session: &Session) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Overwrites an existing session.
    fn update(&self, session: &Session) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads one session.
    fn get(&self, id: &SessionId)
        -> impl Future<Output = RepositoryResult<Option<Session>>> + Send;

    /// All sessions, newest first.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<Session>>> + Send;

    /// Deletes a session and everything derived from it (frames, transcript, notes, index).
    fn delete(&self, id: &SessionId) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Inserts or replaces the track of `track.kind` for the session.
    fn save_audio_track(
        &self,
        track: &AudioTrack,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// The session's audio tracks.
    fn list_audio_tracks(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Vec<AudioTrack>>> + Send;
}

/// Persistence of captured frames.
pub trait FrameRepository: Send + Sync {
    /// Inserts a frame.
    fn add(&self, frame: &Frame) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads one frame.
    fn get(&self, id: &FrameId) -> impl Future<Output = RepositoryResult<Option<Frame>>> + Send;

    /// The session's frames ordered by timestamp.
    fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Vec<Frame>>> + Send;

    /// Sets or clears the vision description.
    fn set_description(
        &self,
        id: &FrameId,
        description: Option<&str>,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Deletes one frame row (the image file is the caller's concern).
    fn delete(&self, id: &FrameId) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of transcripts.
pub trait TranscriptRepository: Send + Sync {
    /// Replaces the session's whole transcript (an empty slice clears it) and refreshes the
    /// search index.
    fn replace_for_session(
        &self,
        session_id: &SessionId,
        segments: &[TranscriptSegment],
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// The session's segments ordered by start time.
    fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Vec<TranscriptSegment>>> + Send;
}

/// Persistence of notes and their action items.
pub trait NoteRepository: Send + Sync {
    /// Inserts or replaces the note and replaces its action items with `action_items`;
    /// refreshes the search index.
    fn save(
        &self,
        note: &Note,
        action_items: &[ActionItem],
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads one note.
    fn get(&self, id: &NoteId) -> impl Future<Output = RepositoryResult<Option<Note>>> + Send;

    /// The most recently created note of the session.
    fn latest_for_session(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Option<Note>>> + Send;

    /// Deletes a note with its action items.
    fn delete(&self, id: &NoteId) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// The note's action items in their original order.
    fn list_action_items(
        &self,
        note_id: &NoteId,
    ) -> impl Future<Output = RepositoryResult<Vec<ActionItem>>> + Send;

    /// Inserts or replaces a single action item (the owning note must exist).
    fn upsert_action_item(
        &self,
        item: &ActionItem,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Marks an action item done / not done.
    fn set_action_item_done(
        &self,
        id: &ActionItemId,
        done: bool,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Deletes one action item.
    fn delete_action_item(
        &self,
        id: &ActionItemId,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of non-secret variables.
pub trait VariableRepository: Send + Sync {
    /// Inserts or replaces by id. A different variable already using the key is a conflict.
    fn upsert(&self, variable: &Variable) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads by id.
    fn get(
        &self,
        id: &VariableId,
    ) -> impl Future<Output = RepositoryResult<Option<Variable>>> + Send;

    /// Loads by unique key.
    fn get_by_key(
        &self,
        key: &str,
    ) -> impl Future<Output = RepositoryResult<Option<Variable>>> + Send;

    /// All variables ordered by key.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<Variable>>> + Send;

    /// Deletes by id.
    fn delete(&self, id: &VariableId) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of secret metadata. Secret **values** are never handled here (ADR-007).
pub trait SecretMetadataRepository: Send + Sync {
    /// Inserts new metadata; duplicate id or name is a conflict.
    fn create(&self, secret: &SecretRef) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Updates name/description of existing metadata.
    fn update(&self, secret: &SecretRef) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads by id.
    fn get(
        &self,
        id: &SecretId,
    ) -> impl Future<Output = RepositoryResult<Option<SecretRef>>> + Send;

    /// All secrets ordered by name.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<SecretRef>>> + Send;

    /// Deletes the metadata. Profiles referencing it lose their reference.
    fn delete(&self, id: &SecretId) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of AI provider profiles.
pub trait ProviderProfileRepository: Send + Sync {
    /// Inserts or replaces by id. A dangling `secret_ref` is a conflict.
    fn upsert(
        &self,
        profile: &ProviderProfile,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Loads by id.
    fn get(
        &self,
        id: &ProviderProfileId,
    ) -> impl Future<Output = RepositoryResult<Option<ProviderProfile>>> + Send;

    /// All profiles ordered by display name.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<ProviderProfile>>> + Send;

    /// Deletes the profile and any task assignments pointing at it.
    fn delete(&self, id: &ProviderProfileId) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of the task-to-profile assignments.
pub trait TaskAssignmentRepository: Send + Sync {
    /// Assigns (or re-assigns) a task. A dangling profile id is a conflict.
    fn set(&self, assignment: &TaskAssignment)
        -> impl Future<Output = RepositoryResult<()>> + Send;

    /// The assignment of one task, if any.
    fn get(
        &self,
        task: TaskKind,
    ) -> impl Future<Output = RepositoryResult<Option<TaskAssignment>>> + Send;

    /// All assignments.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<TaskAssignment>>> + Send;

    /// Removes the assignment of a task (no error if there is none).
    fn clear(&self, task: TaskKind) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of application settings.
pub trait SettingsRepository: Send + Sync {
    /// The current settings; fields never written fall back to the domain defaults.
    fn load(&self) -> impl Future<Output = RepositoryResult<AppSettings>> + Send;

    /// Applies a partial update (validated by the domain) and returns the resulting settings.
    /// Fields not present in the patch are preserved.
    fn update(
        &self,
        patch: &SettingsPatch,
    ) -> impl Future<Output = RepositoryResult<AppSettings>> + Send;
}

/// Which kind of content a search hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// A transcript segment.
    Transcript,
    /// A note (summary, decisions, open questions).
    Note,
}

/// One full-text search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Session the match belongs to.
    pub session_id: SessionId,
    /// What matched.
    pub source: SearchSource,
    /// Start offset in milliseconds for transcript hits.
    pub start_ms: Option<u64>,
    /// Beginning of the matching text, for display.
    pub excerpt: String,
}

/// Full-text search over transcripts and notes.
pub trait SearchIndex: Send + Sync {
    /// Finds content matching every word of `query` (prefix match, case-insensitive,
    /// Turkish-aware), best matches first, at most `limit` hits. Free text is accepted: no
    /// search-syntax characters need escaping and an empty query yields no hits.
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = RepositoryResult<Vec<SearchHit>>> + Send;
}
