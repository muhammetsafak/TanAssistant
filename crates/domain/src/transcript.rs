//! Transcript artifacts.

use crate::ids::SessionId;
use crate::language::LanguageCode;
use crate::media::TrackKind;

/// One transcribed utterance. The text stays in the language it was spoken in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSegment {
    /// Owning session.
    pub session_id: SessionId,
    /// Which audio track the segment came from.
    pub track: TrackKind,
    /// Start offset from the session start, in milliseconds.
    pub start_ms: u64,
    /// End offset from the session start, in milliseconds.
    pub end_ms: u64,
    /// Detected or forced spoken language, when known.
    pub language: Option<LanguageCode>,
    /// The transcribed text.
    pub text: String,
    /// Speaker label (e.g. `Me`, `Speaker 2`), when known.
    pub speaker: Option<String>,
}
