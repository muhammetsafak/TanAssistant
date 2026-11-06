//! Read-side use cases: listing, searching and loading a whole session.

use std::sync::Arc;

use tan_domain::{
    ActionItem, AudioTrack, EpochMs, Frame, Note, ProcessingJob, Session, SessionId, SessionStatus,
    TranscriptSegment,
};

use super::{AudioRetentionRepository, SessionsError, SessionsResult};
use crate::ports::{
    FrameRepository, NoteRepository, SearchHit, SearchIndex, SessionRepository,
    TranscriptRepository,
};
use crate::processing::ProcessingJobRepository;

/// Sessions per page unless the caller asks otherwise.
pub const DEFAULT_PAGE_SIZE: usize = 50;
/// Largest page a caller can get.
pub const MAX_PAGE_SIZE: usize = 200;
/// Longest accepted search text, in characters.
const MAX_QUERY_CHARS: usize = 200;
/// Most search hits one search reads from the index.
const MAX_SEARCH_HITS: usize = 200;

/// Which sessions to list. Every set field must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFilter {
    /// Only sessions in this status.
    pub status: Option<SessionStatus>,
    /// Only sessions whose title contains this text (case-insensitive).
    pub title_contains: Option<String>,
    /// Only sessions that started at or after this instant.
    pub started_from: Option<EpochMs>,
    /// Only sessions that started before this instant.
    pub started_before: Option<EpochMs>,
}

impl SessionFilter {
    fn matches(&self, session: &Session) -> bool {
        if self.status.is_some_and(|status| status != session.status()) {
            return false;
        }
        if self
            .started_from
            .is_some_and(|from| session.started_at < from)
        {
            return false;
        }
        if self
            .started_before
            .is_some_and(|before| session.started_at >= before)
        {
            return false;
        }
        match self.title_contains.as_deref().map(str::trim) {
            Some(needle) if !needle.is_empty() => session
                .title
                .to_lowercase()
                .contains(&needle.to_lowercase()),
            _ => true,
        }
    }
}

/// Which slice of the (newest-first) list to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Sessions to skip.
    pub offset: usize,
    /// Sessions to return, `1..=MAX_PAGE_SIZE` (clamped).
    pub limit: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: DEFAULT_PAGE_SIZE,
        }
    }
}

/// A session in a list, with what the list needs to show about its note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    /// The session.
    pub session: Session,
    /// Whether a note has been generated.
    pub has_note: bool,
    /// Action items in the note.
    pub action_items_total: usize,
    /// Of those, the ones marked done.
    pub action_items_done: usize,
}

/// One page of sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPage {
    /// The sessions of this page, newest first.
    pub items: Vec<SessionListItem>,
    /// Sessions matching the filter in all pages.
    pub total: usize,
    /// The offset that was applied.
    pub offset: usize,
    /// The page size that was applied (after clamping).
    pub limit: usize,
}

/// Lists sessions, newest first, filtered and paginated.
pub struct ListSessions<S, N> {
    sessions: Arc<S>,
    notes: Arc<N>,
}

impl<S: SessionRepository, N: NoteRepository> ListSessions<S, N> {
    /// Creates the use case.
    #[must_use]
    pub fn new(sessions: Arc<S>, notes: Arc<N>) -> Self {
        Self { sessions, notes }
    }

    /// Runs it.
    pub async fn execute(&self, filter: &SessionFilter, page: Page) -> SessionsResult<SessionPage> {
        let limit = page.limit.clamp(1, MAX_PAGE_SIZE);
        let matching: Vec<Session> = self
            .sessions
            .list()
            .await?
            .into_iter()
            .filter(|session| filter.matches(session))
            .collect();
        let total = matching.len();
        let mut items = Vec::new();
        for session in matching.into_iter().skip(page.offset).take(limit) {
            let (has_note, action_items_total, action_items_done) =
                match self.notes.latest_for_session(&session.id).await? {
                    Some(note) => {
                        let actions = self.notes.list_action_items(&note.id).await?;
                        (
                            true,
                            actions.len(),
                            actions.iter().filter(|item| item.done).count(),
                        )
                    }
                    None => (false, 0, 0),
                };
            items.push(SessionListItem {
                session,
                has_note,
                action_items_total,
                action_items_done,
            });
        }
        Ok(SessionPage {
            items,
            total,
            offset: page.offset,
            limit,
        })
    }
}

/// A session with the hits that matched a search, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSearchResult {
    /// The session.
    pub session: Session,
    /// Its matching transcript segments and note text.
    pub hits: Vec<SearchHit>,
}

/// Full-text search over transcripts and notes, grouped by session.
pub struct SearchSessions<S, I> {
    sessions: Arc<S>,
    index: Arc<I>,
}

impl<S: SessionRepository, I: SearchIndex> SearchSessions<S, I> {
    /// Creates the use case.
    #[must_use]
    pub fn new(sessions: Arc<S>, index: Arc<I>) -> Self {
        Self { sessions, index }
    }

    /// Finds sessions matching every word of `query`. Sessions are ordered by their best hit;
    /// a blank query finds nothing.
    pub async fn execute(&self, query: &str) -> SessionsResult<Vec<SessionSearchResult>> {
        let query = query.trim();
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(SessionsError::Invalid(format!(
                "the search text is longer than {MAX_QUERY_CHARS} characters"
            )));
        }
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let hits = self.index.search(query, MAX_SEARCH_HITS).await?;
        let mut grouped: Vec<(SessionId, Vec<SearchHit>)> = Vec::new();
        for hit in hits {
            match grouped.iter_mut().find(|(id, _)| *id == hit.session_id) {
                Some((_, bucket)) => bucket.push(hit),
                None => grouped.push((hit.session_id.clone(), vec![hit])),
            }
        }
        let mut results = Vec::with_capacity(grouped.len());
        for (id, hits) in grouped {
            // A hit can outlive its session only through a race with a deletion; skip it.
            if let Some(session) = self.sessions.get(&id).await? {
                results.push(SessionSearchResult { session, hits });
            }
        }
        Ok(results)
    }
}

/// An audio track and whether the retention policy deleted its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackState {
    /// The stored track.
    pub track: AudioTrack,
    /// When its file was deleted; `None` while the file is kept.
    pub deleted_at: Option<EpochMs>,
}

/// A note with its action items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteWithItems {
    /// The note.
    pub note: Note,
    /// Its action items, in order.
    pub action_items: Vec<ActionItem>,
}

/// Everything the session screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDetail {
    /// The session.
    pub session: Session,
    /// Its audio tracks.
    pub audio_tracks: Vec<AudioTrackState>,
    /// Its frames in time order, with their descriptions.
    pub frames: Vec<Frame>,
    /// The transcript in time order.
    pub transcript: Vec<TranscriptSegment>,
    /// The latest note, if one was generated.
    pub note: Option<NoteWithItems>,
    /// The processing job, if the session was ever processed.
    pub job: Option<ProcessingJob>,
}

/// Loads a session with its tracks, frames, transcript, note and job.
pub struct GetSessionDetail<S, Fr, T, N, J, R> {
    sessions: Arc<S>,
    frames: Arc<Fr>,
    transcripts: Arc<T>,
    notes: Arc<N>,
    jobs: Arc<J>,
    retention: Arc<R>,
}

impl<S, Fr, T, N, J, R> GetSessionDetail<S, Fr, T, N, J, R>
where
    S: SessionRepository,
    Fr: FrameRepository,
    T: TranscriptRepository,
    N: NoteRepository,
    J: ProcessingJobRepository,
    R: AudioRetentionRepository,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(
        sessions: Arc<S>,
        frames: Arc<Fr>,
        transcripts: Arc<T>,
        notes: Arc<N>,
        jobs: Arc<J>,
        retention: Arc<R>,
    ) -> Self {
        Self {
            sessions,
            frames,
            transcripts,
            notes,
            jobs,
            retention,
        }
    }

    /// Runs it.
    pub async fn execute(&self, id: &SessionId) -> SessionsResult<SessionDetail> {
        let session = self
            .sessions
            .get(id)
            .await?
            .ok_or(SessionsError::NotFound("session"))?;
        let deleted = self.retention.deleted_tracks(id).await?;
        let audio_tracks = self
            .sessions
            .list_audio_tracks(id)
            .await?
            .into_iter()
            .map(|track| AudioTrackState {
                deleted_at: deleted
                    .iter()
                    .find(|(kind, _)| *kind == track.kind)
                    .map(|(_, at)| *at),
                track,
            })
            .collect();
        let note = match self.notes.latest_for_session(id).await? {
            Some(note) => {
                let action_items = self.notes.list_action_items(&note.id).await?;
                Some(NoteWithItems { note, action_items })
            }
            None => None,
        };
        Ok(SessionDetail {
            audio_tracks,
            frames: self.frames.list_for_session(id).await?,
            transcript: self.transcripts.list_for_session(id).await?,
            note,
            job: self.jobs.get(id).await?,
            session,
        })
    }
}
