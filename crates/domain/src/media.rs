//! Captured media: audio tracks and screen frames.
//!
//! File paths are strings relative to the session directory (`audio/...`, `frames/...`) so the
//! data directory can be moved without rewriting rows.

use crate::ids::{FrameId, SessionId};

string_enum! {
    /// Which side of the conversation a track carries (ADR-009).
    pub enum TrackKind {
        /// The user's microphone.
        Microphone => "microphone",
        /// System/loopback audio: the other participants.
        System => "system",
    }
}

/// One recorded audio file of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Owning session.
    pub session_id: SessionId,
    /// Microphone or system audio. A session has at most one track per kind.
    pub kind: TrackKind,
    /// Path of the audio file, relative to the session directory.
    pub path: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// A stored screen capture (only visibly changed frames are kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Identity.
    pub id: FrameId,
    /// Owning session.
    pub session_id: SessionId,
    /// Offset from the session start, in milliseconds.
    pub timestamp_ms: u64,
    /// Path of the image file, relative to the session directory.
    pub path: String,
    /// 64-bit perceptual hash used for change detection.
    pub perceptual_hash: u64,
    /// Vision-model description; filled in by the pipeline later.
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::TrackKind;

    #[test]
    fn track_kind_text_round_trips() {
        for &kind in TrackKind::ALL {
            assert_eq!(kind.as_str().parse::<TrackKind>().unwrap(), kind);
        }
    }
}
