//! People: the application-wide directory of the individuals meetings are about, and who took
//! part in which session (ADR-014).

use crate::ids::{PersonId, SessionId};
use crate::EpochMs;

/// One entry of the person directory.
///
/// Two names that differ only in case, spacing or Turkish letter form are the same person; the
/// application layer decides that with its name normalizer and the storage keeps the result
/// unique. The domain only carries the name as it was first written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// Identity.
    pub id: PersonId,
    /// The name as shown to the user.
    pub display_name: String,
    /// Whether this is the person who makes the recordings ("me"). Exactly one person is.
    pub is_self: bool,
    /// When the person entered the directory.
    pub created_at: EpochMs,
}

string_enum! {
    /// Where a session participant came from.
    pub enum ParticipantSource {
        /// Extracted by the AI while writing the note; replaced when the note is regenerated.
        Ai => "ai",
        /// Added by the user; never removed by reprocessing.
        Manual => "manual",
    }
}

/// A person who took part in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionParticipant {
    /// The session.
    pub session_id: SessionId,
    /// The participant.
    pub person_id: PersonId,
    /// Who added the participant.
    pub source: ParticipantSource,
}

#[cfg(test)]
mod tests {
    use super::ParticipantSource;

    #[test]
    fn participant_source_round_trips_its_text() {
        for source in ParticipantSource::ALL {
            assert_eq!(
                source.as_str().parse::<ParticipantSource>().unwrap(),
                *source
            );
        }
        assert!("robot".parse::<ParticipantSource>().is_err());
    }
}
