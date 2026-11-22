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
    ActionBrief, ActionItem, ActionItemId, AppSettings, AudioTrack, Frame, FrameId, Note, NoteId,
    Person, PersonId, ProviderProfile, ProviderProfileId, SecretId, SecretRef, Session, SessionId,
    SessionParticipant, SettingsPatch, TaskAssignment, TaskKind, TrackKind, TrackMute,
    TranscriptSegment, Variable, VariableId,
};

use crate::error::RepositoryResult;

/// Persistence of sessions, their audio tracks and the mute intervals of those tracks.
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

    /// Records that `kind` was muted from `start_ms` (active recording time). The interval
    /// stays open (no end) until [`Self::close_track_mute`].
    fn open_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        start_ms: u64,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Ends the open mute interval of `kind` at `end_ms`. Does nothing when `kind` has no
    /// open interval, so it is safe to call for every kind when settling a session.
    fn close_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        end_ms: u64,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// The session's mute intervals, oldest first.
    fn list_track_mutes(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Vec<TrackMute>>> + Send;
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

    /// Saves `note` (as [`save`](Self::save) does), removes every other note of the same
    /// session and replaces the session's AI-extracted participants with `ai_participants`
    /// (participants the user added are kept), all in one transaction: either the session has
    /// exactly this note with its people, or nothing changed. A person that is not in the
    /// directory is a conflict.
    fn replace_for_session(
        &self,
        note: &Note,
        action_items: &[ActionItem],
        ai_participants: &[PersonId],
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

    /// Loads one action item.
    fn get_action_item(
        &self,
        id: &ActionItemId,
    ) -> impl Future<Output = RepositoryResult<Option<ActionItem>>> + Send;

    /// Deletes one action item.
    fn delete_action_item(
        &self,
        id: &ActionItemId,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Sets the owner of an action item (`None` clears it) and marks the choice as the user's
    /// (`owner_edited`). A person that is not in the directory is a conflict.
    fn set_action_item_owner(
        &self,
        id: &ActionItemId,
        owner: Option<&PersonId>,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of the briefs written for action items. A brief goes with its action item.
pub trait ActionBriefRepository: Send + Sync {
    /// The brief of an action item, if one was generated.
    fn get(
        &self,
        action_item_id: &ActionItemId,
    ) -> impl Future<Output = RepositoryResult<Option<ActionBrief>>> + Send;

    /// Stores the brief, replacing the action item's earlier one. An action item that does not
    /// exist (any more) is a conflict.
    fn save(&self, brief: &ActionBrief) -> impl Future<Output = RepositoryResult<()>> + Send;
}

/// Persistence of the person directory and of the participants of sessions.
pub trait PersonRepository: Send + Sync {
    /// Everyone in the directory: the "me" person first, the rest by normalized name.
    fn list(&self) -> impl Future<Output = RepositoryResult<Vec<Person>>> + Send;

    /// Loads one person.
    fn get(&self, id: &PersonId) -> impl Future<Output = RepositoryResult<Option<Person>>> + Send;

    /// The "me" person: the one who makes the recordings. It always exists.
    fn self_person(&self) -> impl Future<Output = RepositoryResult<Person>> + Send;

    /// The person whose normalized name is `normalized_name`; when there is none, `candidate`
    /// is stored (with that normalized name) and returned. The caller computes the normalized
    /// name (see the application's `people` module) so that there is one rule.
    fn find_or_create(
        &self,
        normalized_name: &str,
        candidate: &Person,
    ) -> impl Future<Output = RepositoryResult<Person>> + Send;

    /// The participants of a session in the order they were added.
    fn list_participants(
        &self,
        session_id: &SessionId,
    ) -> impl Future<Output = RepositoryResult<Vec<SessionParticipant>>> + Send;

    /// Adds a participant. A person who already took part keeps the row, except that a
    /// [`Manual`](tan_domain::ParticipantSource::Manual) addition upgrades an AI row (the
    /// user's choice must survive reprocessing). An unknown session or person is a conflict.
    fn add_participant(
        &self,
        participant: &SessionParticipant,
    ) -> impl Future<Output = RepositoryResult<()>> + Send;

    /// Removes a participant; [`NotFound`](crate::RepositoryError::NotFound) when the person
    /// did not take part.
    fn remove_participant(
        &self,
        session_id: &SessionId,
        person_id: &PersonId,
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
