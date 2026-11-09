//! Audio export of a session: its tracks mixed into one WAV file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tan_domain::SessionStatus;

use super::export::{check_destination, ExportExtension};
use super::{SessionDetail, SessionFileStore, SessionsError, SessionsResult};
use crate::ports::{AudioMixError, AudioMixer};
use crate::transcription::SessionAudioFiles;

/// The folder of a session that holds its audio; a stored track path outside it is a corrupt
/// row and is never read.
const AUDIO_DIR: &str = "audio";

/// Mixes the audio tracks of a session into one WAV file the user picked.
///
/// Tracks whose audio the retention policy deleted, and tracks whose file is gone, are left
/// out. A session without any usable track is refused with [`SessionsError::NoAudio`].
pub struct ExportSessionAudio<M, W, D> {
    mixer: Arc<M>,
    files: Arc<W>,
    paths: Arc<D>,
}

impl<M, W, D> ExportSessionAudio<M, W, D>
where
    M: AudioMixer,
    W: SessionFileStore,
    D: SessionAudioFiles,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(mixer: Arc<M>, files: Arc<W>, paths: Arc<D>) -> Self {
        Self {
            mixer,
            files,
            paths,
        }
    }

    /// The files of the tracks that can be exported, in the order of the session's tracks.
    fn usable_tracks(&self, detail: &SessionDetail) -> SessionsResult<Vec<PathBuf>> {
        let id = &detail.session.id;
        let mut tracks = Vec::new();
        for state in &detail.audio_tracks {
            if state.deleted_at.is_some() {
                continue;
            }
            let relative = state.track.path.as_str();
            if !Path::new(relative).starts_with(AUDIO_DIR) {
                continue;
            }
            if !self.files.audio_exists(id, relative) {
                continue;
            }
            tracks.push(self.paths.audio_path(id, relative)?);
        }
        Ok(tracks)
    }

    /// Mixes the session's tracks into `destination` and returns the path written.
    ///
    /// `destination` must be absolute, free of `..`, on a local drive (no UNC or device paths)
    /// and end in `.wav`. The session must not be recording. The file is replaced if it exists;
    /// a failure leaves no half-written file. Blocking: run it off the async workers.
    pub fn execute_to(
        &self,
        detail: &SessionDetail,
        destination: &Path,
    ) -> SessionsResult<PathBuf> {
        if detail.session.status() == SessionStatus::Recording {
            return Err(SessionsError::Recording);
        }
        check_destination(destination, ExportExtension::Wav)?;
        let tracks = self.usable_tracks(detail)?;
        if tracks.is_empty() {
            return Err(SessionsError::NoAudio);
        }
        self.mixer
            .mix_to_wav(&tracks, destination)
            .map_err(|error| match error {
                AudioMixError::NoTracks | AudioMixError::Empty => SessionsError::NoAudio,
                other => SessionsError::Files(std::io::Error::other(other)),
            })?;
        Ok(destination.to_path_buf())
    }
}
