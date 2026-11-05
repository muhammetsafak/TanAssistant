//! Note artifacts: the generated summary and its action items.

use crate::ids::{ActionItemId, FrameId, NoteId, SessionId};
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
    /// Who is responsible, when known.
    pub owner: Option<String>,
    /// Due date, when known.
    pub due: Option<EpochMs>,
    /// Whether the user marked it completed.
    pub done: bool,
}
