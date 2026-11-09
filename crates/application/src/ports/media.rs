//! Media processing ports: turning stored recordings into new files.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Why tracks could not be mixed into a file.
#[derive(Debug)]
pub enum AudioMixError {
    /// No track was given.
    NoTracks,
    /// The tracks hold no audio at all.
    Empty,
    /// A track could not be read (missing, not a WAV file, damaged).
    Source(String),
    /// The result could not be written.
    Destination(io::Error),
}

impl fmt::Display for AudioMixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTracks => f.write_str("there is no audio track to mix"),
            Self::Empty => f.write_str("the audio tracks are empty"),
            Self::Source(why) => write!(f, "an audio track could not be read: {why}"),
            Self::Destination(error) => write!(f, "the mixed audio could not be written: {error}"),
        }
    }
}

impl Error for AudioMixError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Destination(error) => Some(error),
            _ => None,
        }
    }
}

/// Mixes the audio tracks of a recording into one file.
pub trait AudioMixer: Send + Sync {
    /// Mixes `tracks` (one or two WAV files; any sample rate and channel count) into a single
    /// mono 16-bit PCM WAV file at `destination`, at the highest sample rate among the tracks.
    /// A track shorter than the others is completed with silence; with one track, that track
    /// is written as it is (mono, at its own rate).
    ///
    /// The file is replaced if it exists, and a failure never leaves a half-written file (or
    /// a leftover temporary one) behind. The destination's folder must exist already. The work
    /// is blocking: call it off the async workers.
    fn mix_to_wav(&self, tracks: &[PathBuf], destination: &Path) -> Result<(), AudioMixError>;
}
