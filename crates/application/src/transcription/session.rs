//! `TranscribeSession`: transcribe every track of a session and store the merged transcript.

use std::path::PathBuf;
use std::sync::Arc;

use tan_domain::{
    AudioTrack, LanguageCode, LanguageSetting, ProviderProfile, ProviderProfileId, Session,
    SessionId, TaskKind, TrackKind, TranscriptSegment,
};

use super::chunking::{self, ChunkError};
use super::{
    speaker_label, AudioChunker, ChunkProgress, ChunkSettings, LocalEngineStatus, LocalTranscriber,
    SessionAudioFiles, TranscribeError, TranscribeProgress, TranscribeResult, TranscribeSummary,
    TranscriptionEngine, TranscriptionEstimate, WHISPER_WINDOW_MS,
};
use crate::ports::{
    AiError, AudioSource, Capabilities, ProviderFactory, ProviderProfileRepository,
    SessionRepository, TaskAssignmentRepository, TranscriptRepository, TranscriptSegmentDto,
    TranscriptionModel, TranscriptionRequest,
};

/// Everything [`TranscribeSession`] depends on.
pub struct TranscribePorts<S, T, A, P, F, L, C, D> {
    /// Sessions and their audio tracks.
    pub sessions: Arc<S>,
    /// Where the transcript is stored (and indexed for search).
    pub transcripts: Arc<T>,
    /// Which profile performs transcription.
    pub assignments: Arc<A>,
    /// The provider profiles.
    pub profiles: Arc<P>,
    /// Builds the cloud adapter of a profile.
    pub factory: Arc<F>,
    /// The local whisper engine.
    pub local: Arc<L>,
    /// Splits audio for cloud uploads.
    pub chunker: Arc<C>,
    /// Maps stored track paths to files.
    pub files: Arc<D>,
}

/// How a track reaches the engine.
#[derive(Clone, Copy)]
enum Delivery {
    /// The engine reads the whole file itself (local whisper).
    WholeFile,
    /// The speech is cut into short compressed pieces that are uploaded a few at a time (cloud
    /// providers).
    Chunked,
}

/// Transcribes a session with the assigned provider, or local whisper when none is assigned
/// (ADR-003). Microphone segments are labelled `me`, system-audio segments `others`; both
/// tracks are merged by time and replace the session's stored transcript.
///
/// It does not change the session status: whoever runs it (the processing pipeline) owns the
/// lifecycle.
pub struct TranscribeSession<S, T, A, P, F, L, C, D> {
    ports: TranscribePorts<S, T, A, P, F, L, C, D>,
    chunking: ChunkSettings,
}

impl<S, T, A, P, F, L, C, D> TranscribeSession<S, T, A, P, F, L, C, D>
where
    S: SessionRepository,
    T: TranscriptRepository,
    A: TaskAssignmentRepository,
    P: ProviderProfileRepository,
    F: ProviderFactory,
    L: LocalTranscriber,
    C: AudioChunker,
    D: SessionAudioFiles,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(ports: TranscribePorts<S, T, A, P, F, L, C, D>) -> Self {
        Self {
            ports,
            chunking: ChunkSettings::default(),
        }
    }

    /// Changes how a cloud provider is fed (piece length, pieces in flight, retrying). Local
    /// whisper is not affected: it reads the whole file itself.
    #[must_use]
    pub fn with_chunking(mut self, chunking: ChunkSettings) -> Self {
        self.chunking = chunking;
        self
    }

    /// Transcribes the session and stores the transcript.
    ///
    /// Each finished track is stored at once (merged with the ones before it), so when a later
    /// track fails the earlier work is not lost; the error is still returned.
    pub async fn execute(&self, session_id: &SessionId) -> TranscribeResult<TranscribeSummary> {
        self.execute_with_progress(session_id, &|_| {}).await
    }

    /// Like [`Self::execute`], reporting [`TranscribeProgress`] to `on_progress`.
    pub async fn execute_with_progress(
        &self,
        session_id: &SessionId,
        on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
    ) -> TranscribeResult<TranscribeSummary> {
        self.execute_with_chunk_progress(session_id, on_progress, &|_| {})
            .await
    }

    /// Like [`Self::execute_with_progress`], also reporting [`ChunkProgress`] to `on_chunk` while
    /// a cloud provider works through the pieces of a track (local whisper sends none).
    pub async fn execute_with_chunk_progress(
        &self,
        session_id: &SessionId,
        on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
        on_chunk: &(dyn Fn(ChunkProgress) + Send + Sync),
    ) -> TranscribeResult<TranscribeSummary> {
        self.execute_with_profile(session_id, None, on_progress, on_chunk)
            .await
    }

    /// Like [`Self::execute_with_chunk_progress`], with the cloud provider of `profile` doing
    /// the work in this call only. Nothing is assigned: the general transcription assignment
    /// (or local whisper) is untouched and applies again on the next call. `None` behaves like
    /// [`Self::execute_with_chunk_progress`].
    ///
    /// A profile that does not exist is an [`AiError::InvalidRequest`] and one that cannot
    /// transcribe audio an [`AiError::Unsupported`], both as [`TranscribeError::Provider`].
    pub async fn execute_with_profile(
        &self,
        session_id: &SessionId,
        profile: Option<&ProviderProfileId>,
        on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
        on_chunk: &(dyn Fn(ChunkProgress) + Send + Sync),
    ) -> TranscribeResult<TranscribeSummary> {
        let (session, tracks) = self.load(session_id).await?;

        let chosen = match profile {
            Some(profile_id) => Some(self.requested_profile(profile_id).await?),
            None => self.assigned_profile().await?,
        };
        let (segment_count, engine) = if let Some(profile) = chosen {
            let model = self
                .ports
                .factory
                .transcription(&profile)
                .await
                .map_err(TranscribeError::Provider)?;
            let count = self
                .transcribe_tracks(
                    &model,
                    Delivery::Chunked,
                    &session,
                    &tracks,
                    on_progress,
                    on_chunk,
                )
                .await?;
            let engine = TranscriptionEngine::Provider {
                profile_id: profile.id,
                kind: profile.kind,
            };
            (count, engine)
        } else {
            if self.ports.local.status().await == LocalEngineStatus::ModelMissing {
                return Err(TranscribeError::LocalModelMissing);
            }
            let count = self
                .transcribe_tracks(
                    &*self.ports.local,
                    Delivery::WholeFile,
                    &session,
                    &tracks,
                    on_progress,
                    on_chunk,
                )
                .await?;
            (count, TranscriptionEngine::LocalWhisper)
        };

        Ok(TranscribeSummary {
            session_id: session_id.clone(),
            segment_count,
            engine,
        })
    }

    /// Forecasts how long [`Self::execute`] would take. With the local engine this calibrates
    /// the first time (a few seconds, remembered afterwards) and counts the speech in the
    /// tracks; a cloud provider gets no time estimate.
    pub async fn estimate(
        &self,
        session_id: &SessionId,
    ) -> TranscribeResult<TranscriptionEstimate> {
        let (session, tracks) = self.load(session_id).await?;
        let audio_ms = tracks.iter().map(|t| t.duration_ms).sum();

        if let Some(profile) = self.assigned_profile().await? {
            return Ok(TranscriptionEstimate {
                engine: TranscriptionEngine::Provider {
                    profile_id: profile.id,
                    kind: profile.kind,
                },
                estimated_seconds: None,
                backend: "cloud".to_owned(),
                audio_ms,
                speech_ms: None,
            });
        }
        if self.ports.local.status().await == LocalEngineStatus::ModelMissing {
            return Err(TranscribeError::LocalModelMissing);
        }
        let speed = self
            .ports
            .local
            .speed()
            .await
            .map_err(TranscribeError::Provider)?;
        let mut speech_ms = 0_u64;
        let mut windows = 0_u64;
        for track in &tracks {
            let path = self.audio_file(&session, track)?;
            let track_speech = self.ports.local.speech_ms(&path).await?;
            speech_ms += track_speech;
            windows += track_speech.div_ceil(WHISPER_WINDOW_MS);
        }
        // Rounded up: a forecast that errs short is worse than one that errs long.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let estimated_seconds = (windows as f64 * speed.seconds_per_window).ceil() as u64;
        Ok(TranscriptionEstimate {
            engine: TranscriptionEngine::LocalWhisper,
            estimated_seconds: Some(estimated_seconds),
            backend: speed.backend,
            audio_ms,
            speech_ms: Some(speech_ms),
        })
    }

    async fn load(&self, session_id: &SessionId) -> TranscribeResult<(Session, Vec<AudioTrack>)> {
        let session = self
            .ports
            .sessions
            .get(session_id)
            .await?
            .ok_or(TranscribeError::NotFound)?;
        let mut tracks = self.ports.sessions.list_audio_tracks(session_id).await?;
        if tracks.is_empty() {
            return Err(TranscribeError::NoAudio);
        }
        tracks.sort_by_key(|track| track_rank(track.kind));
        Ok((session, tracks))
    }

    fn audio_file(&self, session: &Session, track: &AudioTrack) -> TranscribeResult<PathBuf> {
        self.ports
            .files
            .audio_path(&session.id, &track.path)
            .map_err(|e| TranscribeError::Audio(format!("cannot locate the audio file: {e}")))
    }

    /// The profile assigned to transcription, or `None` when local whisper is to be used (no
    /// assignment, or one whose profile is gone).
    async fn assigned_profile(&self) -> TranscribeResult<Option<ProviderProfile>> {
        let Some(assignment) = self.ports.assignments.get(TaskKind::Transcription).await? else {
            return Ok(None);
        };
        let Some(profile) = self.ports.profiles.get(&assignment.profile_id).await? else {
            return Ok(None);
        };
        Self::checked_for_transcription(profile).map(Some)
    }

    /// The profile a caller asked for explicitly (for one run), which must exist.
    async fn requested_profile(&self, id: &ProviderProfileId) -> TranscribeResult<ProviderProfile> {
        let profile = self.ports.profiles.get(id).await?.ok_or_else(|| {
            TranscribeError::Provider(AiError::InvalidRequest(
                "the transcription profile does not exist".to_owned(),
            ))
        })?;
        Self::checked_for_transcription(profile)
    }

    fn checked_for_transcription(profile: ProviderProfile) -> TranscribeResult<ProviderProfile> {
        if Capabilities::for_profile(&profile).transcription {
            Ok(profile)
        } else {
            Err(TranscribeError::Provider(AiError::Unsupported(format!(
                "{} cannot transcribe audio",
                profile.kind
            ))))
        }
    }

    /// Transcribes the tracks one after the other and stores the merged result. Returns the
    /// number of segments stored.
    ///
    /// A first transcription is stored after every track, so a later track that fails does not
    /// cost the work already done. When the session already has a transcript (a re-run, maybe
    /// with another model) it is replaced only once all tracks are done: a run that fails half
    /// way must not leave a partial transcript in place of a complete one.
    async fn transcribe_tracks<M: TranscriptionModel>(
        &self,
        model: &M,
        delivery: Delivery,
        session: &Session,
        tracks: &[AudioTrack],
        on_progress: &(dyn Fn(TranscribeProgress) + Send + Sync),
        on_chunk: &(dyn Fn(ChunkProgress) + Send + Sync),
    ) -> TranscribeResult<usize> {
        let language = match &session.language {
            LanguageSetting::Fixed(code) => Some(code.clone()),
            LanguageSetting::Auto => None,
        };
        let store_each_track = self
            .ports
            .transcripts
            .list_for_session(&session.id)
            .await?
            .is_empty();
        let mut all: Vec<TranscriptSegment> = Vec::new();
        for (index, track) in tracks.iter().enumerate() {
            on_progress(TranscribeProgress {
                track: Some(track.kind),
                tracks_done: index,
                tracks_total: tracks.len(),
            });
            let path = self.audio_file(session, track)?;
            let engine_error = |error| TranscribeError::Engine {
                track: track.kind,
                error,
            };

            let dtos: Vec<TranscriptSegmentDto> = match delivery {
                Delivery::WholeFile => {
                    let request = TranscriptionRequest {
                        audio: AudioSource::Path(path),
                        language: language.clone(),
                        duration_ms: Some(track.duration_ms),
                        prompt: None,
                    };
                    model.transcribe(request).await.map_err(engine_error)?
                }
                Delivery::Chunked => {
                    let kind = track.kind;
                    chunking::transcribe_track(
                        model,
                        &*self.ports.chunker,
                        &self.chunking,
                        &path,
                        language.as_ref(),
                        &|done, total| {
                            on_chunk(ChunkProgress {
                                track: kind,
                                chunks_done: done,
                                chunks_total: total,
                            });
                        },
                    )
                    .await
                    .map_err(|error| match error {
                        ChunkError::Audio(error) => TranscribeError::from(error),
                        ChunkError::Engine(error) => engine_error(error),
                    })?
                }
            };

            all.extend(
                dtos.into_iter().filter_map(|dto| {
                    segment_from(&session.id, track.kind, &session.language, dto)
                }),
            );
            all = merge_tracks(all);
            if store_each_track || index + 1 == tracks.len() {
                self.ports
                    .transcripts
                    .replace_for_session(&session.id, &all)
                    .await?;
            }
        }
        on_progress(TranscribeProgress {
            track: None,
            tracks_done: tracks.len(),
            tracks_total: tracks.len(),
        });
        Ok(all.len())
    }
}

/// Builds a stored segment; empty text is dropped.
fn segment_from(
    session_id: &SessionId,
    track: TrackKind,
    session_language: &LanguageSetting,
    dto: TranscriptSegmentDto,
) -> Option<TranscriptSegment> {
    let text = dto.text.trim();
    if text.is_empty() {
        return None;
    }
    let language: Option<LanguageCode> = dto.language.or_else(|| match session_language {
        LanguageSetting::Fixed(code) => Some(code.clone()),
        LanguageSetting::Auto => None,
    });
    Some(TranscriptSegment {
        session_id: session_id.clone(),
        track,
        start_ms: dto.start_ms,
        end_ms: dto.end_ms.max(dto.start_ms),
        language,
        text: text.to_owned(),
        speaker: Some(speaker_label(track).to_owned()),
    })
}

const fn track_rank(track: TrackKind) -> u8 {
    match track {
        TrackKind::Microphone => 0,
        TrackKind::System => 1,
    }
}

/// Puts the segments of all tracks in time order. Segments that start together keep a stable
/// order (shorter first, then microphone before system) so the result is deterministic.
#[must_use]
pub fn merge_tracks(mut segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    segments.sort_by_key(|s| (s.start_ms, s.end_ms, track_rank(s.track)));
    segments
}
