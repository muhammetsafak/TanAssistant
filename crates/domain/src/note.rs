//! Note artifacts: the generated summary and its action items.

use crate::ids::{ActionItemId, FrameId, NoteId, PersonId, ProviderProfileId, SessionId};
use crate::language::LanguageCode;
use crate::EpochMs;

/// Generated notes for a session. Filled in by the processing pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Identity.
    pub id: NoteId,
    /// Owning session.
    pub session_id: SessionId,
    /// Language the note was generated in.
    pub language: LanguageCode,
    /// Summary text.
    pub summary: String,
    /// Decisions taken, in order.
    pub decisions: Vec<String>,
    /// Questions left open, in order.
    pub open_questions: Vec<String>,
    /// Frames the note refers to, in order.
    pub frame_refs: Vec<FrameId>,
    /// When the note was generated.
    pub created_at: EpochMs,
}

/// A task extracted from a note; the user can mark it done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionItem {
    /// Identity.
    pub id: ActionItemId,
    /// Owning note.
    pub note_id: NoteId,
    /// What has to be done.
    pub task: String,
    /// Who is responsible, when known. A person of the directory, never free text.
    pub owner_person_id: Option<PersonId>,
    /// Whether the user chose (or cleared) the owner. Such a choice survives regenerating the
    /// note for an action item whose task text is unchanged.
    pub owner_edited: bool,
    /// Due date, when known.
    pub due: Option<EpochMs>,
    /// Whether the user marked it completed.
    pub done: bool,
}

/// A work brief the AI wrote for one action item: context, details, the decisions and open
/// questions that bear on it, the frames and quotes that matter, and suggested steps. It is
/// read-only for the user (it can be regenerated, never edited) and goes with its action item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBrief {
    /// The action item the brief is about; a brief is unique per action item.
    pub action_item_id: ActionItemId,
    /// Language the brief was written in (the note's language).
    pub language: LanguageCode,
    /// The brief's content as JSON. Its shape belongs to the application layer (the domain has
    /// no serializer), so the domain and the storage only carry it.
    pub content_json: String,
    /// The profile whose model wrote it; `None` when that profile has been deleted since.
    pub provider_profile_id: Option<ProviderProfileId>,
    /// When it was generated.
    pub created_at: EpochMs,
}
