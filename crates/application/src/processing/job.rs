//! `ProcessSession`: runs the pipeline for one session, step by step, resumably.

use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};

use tan_domain::{
    ActionItem, ActionItemId, Note, NoteId, ProcessingJob, ProcessingStep, ProviderProfile,
    ProviderProfileId, Session, SessionId, SessionStatus, TaskKind,
};

use super::chunking::TokenBudget;
use super::describe_frames::describe_frames;
use super::synthesize::{synthesize, SynthesisInput};
use super::{
    ProcessError, ProcessProgress, ProcessResult, ProcessSummary, ProcessingJobRepository,
    SessionTranscriber,
};
use crate::ports::{
    AiError, Capabilities, Clock, FrameRepository, IdGenerator, NoteRepository, ProviderFactory,
    ProviderProfileRepository, SessionRepository, TaskAssignmentRepository, TranscriptRepository,
};
use crate::sessions::SessionFileStore;
use crate::transcription::TranscribeError;

/// Everything [`ProcessSession`] depends on.
pub struct ProcessPorts<S, Fr, T, N, A, P, F, J, W, Q> {
    /// Sessions (status changes, audio tracks).
    pub sessions: Arc<S>,
    /// Frames and their descriptions.
    pub frames: Arc<Fr>,
    /// The transcript.
    pub transcripts: Arc<T>,
    /// Notes and action items (the search index is refreshed by the repository).
    pub notes: Arc<N>,
    /// Which profile performs which task.
    pub assignments: Arc<A>,
    /// The provider profiles.
    pub profiles: Arc<P>,
    /// Builds chat and vision adapters.
    pub factory: Arc<F>,
    /// Processing jobs.
    pub jobs: Arc<J>,
    /// Frame images and audio files.
    pub files: Arc<W>,
    /// Runs the transcription step.
    pub transcriber: Arc<Q>,
    /// Mints ids and prompt delimiters.
    pub ids: Arc<dyn IdGenerator>,
    /// The current time.
    pub clock: Arc<dyn Clock>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked; the data is a plain map.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------------------

/// A cancel request that can interrupt a pending future (a long transcription or a model call).
#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl CancelState {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(waker) = lock(&self.waker).take() {
            waker.wake();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Drives `future` until it finishes or `state` is cancelled; `None` means cancelled (the
/// future is dropped, so whatever it was awaiting is abandoned).
///
/// Dropping stops *waiting*, not the work behind it: an HTTP request is abandoned, but a
/// computation already handed to a blocking thread (local whisper) runs to its end and its
/// result is discarded. Interrupting that needs an abort signal in the engine itself.
async fn abortable<F: Future>(state: &CancelState, future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    poll_fn(|cx| {
        if state.is_cancelled() {
            return Poll::Ready(None);
        }
        *lock(&state.waker) = Some(cx.waker().clone());
        // A cancel that landed between the two checks must not be missed.
        if state.is_cancelled() {
            return Poll::Ready(None);
        }
        future.as_mut().poll(cx).map(Some)
    })
    .await
}

/// Registers a running session; removes it again on drop.
struct ActiveRun<'a> {
    registry: &'a Mutex<HashMap<SessionId, Arc<CancelState>>>,
    id: SessionId,
    state: Arc<CancelState>,
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        lock(self.registry).remove(&self.id);
    }
}

// ---------------------------------------------------------------------------------------
// The use case
// ---------------------------------------------------------------------------------------

/// Whether a run starts over or resumes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A new job: every step runs.
    Fresh,
    /// The existing job continues: finished steps are not repeated.
    Resume,
}

/// How a step ended when it did not fail.
enum StepEnd {
    Done,
    Skipped,
}

/// Share of the whole run each step stands for, as `(start, end)` percent.
const fn weight(step: ProcessingStep) -> (u8, u8) {
    match step {
        ProcessingStep::Transcribe => (0, 50),
        ProcessingStep::DescribeFrames => (50, 70),
        // Steps added later (the enum is non-exhaustive) share the tail.
        _ => (70, 100),
    }
}

fn emit(
    on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    step: ProcessingStep,
    step_percent: u8,
) {
    let step_percent = step_percent.min(100);
    let (start, end) = weight(step);
    let percent = start
        + u8::try_from(u16::from(step_percent) * u16::from(end - start) / 100)
            .unwrap_or(end - start);
    on_progress(ProcessProgress {
        step,
        step_percent,
        percent,
    });
}

fn internal(error: impl std::fmt::Display) -> ProcessError {
    ProcessError::Internal(error.to_string())
}

/// Turns a recorded session into a structured note (see the module docs).
///
/// At most one run per session at a time. A run can be cancelled; a cancelled or failed run
/// leaves the session `Failed` with the job recording how far it got, and
/// [`ProcessSession::retry`] continues from there.
pub struct ProcessSession<S, Fr, T, N, A, P, F, J, W, Q> {
    ports: ProcessPorts<S, Fr, T, N, A, P, F, J, W, Q>,
    active: Mutex<HashMap<SessionId, Arc<CancelState>>>,
    budget_for: fn(&ProviderProfile) -> TokenBudget,
}

impl<S, Fr, T, N, A, P, F, J, W, Q> ProcessSession<S, Fr, T, N, A, P, F, J, W, Q>
where
    S: SessionRepository,
    Fr: FrameRepository,
    T: TranscriptRepository,
    N: NoteRepository,
    A: TaskAssignmentRepository,
    P: ProviderProfileRepository,
    F: ProviderFactory,
    J: ProcessingJobRepository,
    W: SessionFileStore,
    Q: SessionTranscriber,
{
    /// Creates the use case with the default token budgets ([`TokenBudget::for_profile`]).
    #[must_use]
    pub fn new(ports: ProcessPorts<S, Fr, T, N, A, P, F, J, W, Q>) -> Self {
        Self {
            ports,
            active: Mutex::new(HashMap::new()),
            budget_for: TokenBudget::for_profile,
        }
    }

    /// Replaces how the token budget is chosen for a profile (provider/model specific tuning).
    #[must_use]
    pub fn with_budget(mut self, budget_for: fn(&ProviderProfile) -> TokenBudget) -> Self {
        self.budget_for = budget_for;
        self
    }

    /// Processes the session from scratch, replacing any earlier job and note.
    pub async fn execute(
        &self,
        session_id: &SessionId,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        self.execute_with_profile(session_id, None, on_progress)
            .await
    }

    /// Like [`Self::execute`], transcribing with the cloud provider of `transcription_profile`
    /// in this run only (`None`: the general assignment, local whisper by default). The choice
    /// is kept in the run's job, never in the assignments, so the next run is unaffected.
    ///
    /// Fails with [`ProcessError::UnknownTranscriptionProfile`] when the profile does not exist
    /// and with a [`ProcessError::Transcribe`] provider error when it cannot transcribe audio;
    /// in both cases nothing has been changed.
    pub async fn execute_with_profile(
        &self,
        session_id: &SessionId,
        transcription_profile: Option<&ProviderProfileId>,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        self.run(session_id, Mode::Fresh, transcription_profile, on_progress)
            .await
    }

    /// Continues a failed or cancelled run: finished steps are kept, the rest runs again. With
    /// no earlier job this is the same as [`Self::execute`].
    pub async fn retry(
        &self,
        session_id: &SessionId,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        self.retry_with_profile(session_id, None, on_progress).await
    }

    /// Like [`Self::retry`]. A `transcription_profile` replaces the one the job was started
    /// with; without one the job keeps its own, so a resumed run transcribes with the profile
    /// the user chose for it (and not silently with another engine). If that profile has been
    /// deleted since, the general assignment is used.
    pub async fn retry_with_profile(
        &self,
        session_id: &SessionId,
        transcription_profile: Option<&ProviderProfileId>,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        self.run(session_id, Mode::Resume, transcription_profile, on_progress)
            .await
    }

    /// Cancels the session's running run. Returns whether there was one.
    ///
    /// The run stops at once as far as the caller is concerned (the session becomes `Failed`,
    /// the interrupted step is pending again). Cloud calls are abandoned; a local whisper
    /// transcription that is already running keeps using the CPU until it finishes, and its
    /// output is discarded. Starting a retry right away can therefore overlap with it.
    pub fn cancel(&self, session_id: &SessionId) -> bool {
        let state = lock(&self.active).get(session_id).cloned();
        state.is_some_and(|state| {
            state.cancel();
            true
        })
    }

    /// Whether this application is processing the session right now.
    #[must_use]
    pub fn is_running(&self, session_id: &SessionId) -> bool {
        lock(&self.active).contains_key(session_id)
    }

    /// Whether this application is processing any session right now. A run counts from the
    /// moment it is registered (before the session turns `Processing`) until it ends, so there
    /// is no gap in which a run has started but this still answers `false`.
    #[must_use]
    pub fn is_any_running(&self) -> bool {
        !lock(&self.active).is_empty()
    }

    /// The session's job, if it was ever processed.
    pub async fn status(&self, session_id: &SessionId) -> ProcessResult<Option<ProcessingJob>> {
        Ok(self.ports.jobs.get(session_id).await?)
    }

    /// Settles sessions a crash left in `Processing` (nothing is running them): their job
    /// steps go back to pending and the session becomes `Failed`, so a retry can resume.
    /// Call it once at startup. Returns how many sessions were settled.
    pub async fn recover_interrupted(&self) -> ProcessResult<usize> {
        let mut settled = 0;
        for mut session in self.ports.sessions.list().await? {
            if session.status() != SessionStatus::Processing || self.is_running(&session.id) {
                continue;
            }
            let now = self.ports.clock.now();
            if let Some(mut job) = self.ports.jobs.get(&session.id).await? {
                job.interrupt(now);
                self.ports.jobs.save(&job).await?;
            }
            session.mark_failed(now).map_err(internal)?;
            self.ports.sessions.update(&session).await?;
            settled += 1;
        }
        Ok(settled)
    }

    // ----- orchestration ---------------------------------------------------------------

    fn register(&self, id: &SessionId) -> ProcessResult<ActiveRun<'_>> {
        let mut active = lock(&self.active);
        if active.contains_key(id) {
            return Err(ProcessError::AlreadyRunning);
        }
        let state = Arc::new(CancelState::default());
        active.insert(id.clone(), Arc::clone(&state));
        Ok(ActiveRun {
            registry: &self.active,
            id: id.clone(),
            state,
        })
    }

    async fn run(
        &self,
        id: &SessionId,
        mode: Mode,
        transcription_profile: Option<&ProviderProfileId>,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        let run = self.register(id)?;
        if let Some(profile_id) = transcription_profile {
            self.check_transcription_profile(profile_id).await?;
        }
        let mut session = self
            .ports
            .sessions
            .get(id)
            .await?
            .ok_or(ProcessError::NotFound)?;
        match session.status() {
            SessionStatus::Recording => return Err(ProcessError::NotReady(session.status())),
            // Left over from a crash (nothing runs it: `register` succeeded): resume as is.
            SessionStatus::Processing => {}
            _ => {
                session.start_processing().map_err(internal)?;
                self.ports.sessions.update(&session).await?;
            }
        }

        // From here on the session must end up `Processed` or `Failed`, whatever happens.
        let outcome = abortable(
            &run.state,
            self.run_job(&session, mode, transcription_profile, on_progress),
        )
        .await;
        let now = self.ports.clock.now();
        match outcome {
            Some(Ok(summary)) => {
                session.mark_processed().map_err(internal)?;
                self.ports.sessions.update(&session).await?;
                Ok(summary)
            }
            Some(Err(error)) => {
                if session.mark_failed(now).is_ok() {
                    // The original error matters more than a failure to record the status.
                    let _ = self.ports.sessions.update(&session).await;
                }
                Err(error)
            }
            None => {
                if let Some(mut job) = self.ports.jobs.get(id).await? {
                    job.interrupt(now);
                    self.ports.jobs.save(&job).await?;
                }
                session.mark_failed(now).map_err(internal)?;
                self.ports.sessions.update(&session).await?;
                Err(ProcessError::Cancelled)
            }
        }
    }

    async fn run_job(
        &self,
        session: &Session,
        mode: Mode,
        transcription_profile: Option<&ProviderProfileId>,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<ProcessSummary> {
        let id = &session.id;
        let now = self.ports.clock.now();
        let resumed = match (mode, self.ports.jobs.get(id).await?) {
            (Mode::Resume, Some(mut job)) => {
                job.prepare_retry(now);
                Some(job)
            }
            _ => None,
        };
        let mut job = if let Some(job) = resumed {
            job
        } else {
            // A fresh run describes every frame again.
            for frame in self.ports.frames.list_for_session(id).await? {
                if frame.description.is_some() {
                    self.ports.frames.set_description(&frame.id, None).await?;
                }
            }
            ProcessingJob::new(id.clone(), now)
        };
        if let Some(profile_id) = transcription_profile {
            job.set_transcription_profile(Some(profile_id.clone()));
        }
        self.ports.jobs.save(&job).await?;

        while let Some(step) = job.next_step() {
            job.start_step(step, self.ports.clock.now())
                .map_err(internal)?;
            self.ports.jobs.save(&job).await?;
            emit(on_progress, step, 0);

            let result = match step {
                ProcessingStep::Transcribe => {
                    let profile = job.transcription_profile().cloned();
                    self.step_transcribe(session, profile, on_progress).await
                }
                ProcessingStep::DescribeFrames => self.step_describe(session, on_progress).await,
                _ => self.step_synthesize(session, on_progress).await,
            };
            let now = self.ports.clock.now();
            match result {
                Ok(StepEnd::Done) => job.complete_step(step, now).map_err(internal)?,
                Ok(StepEnd::Skipped) => job.skip_step(step, now).map_err(internal)?,
                Err(error) => {
                    job.fail_step(step, error.to_string(), now)
                        .map_err(internal)?;
                    // The step's error matters more than a failure to record it.
                    let _ = self.ports.jobs.save(&job).await;
                    return Err(error);
                }
            }
            self.ports.jobs.save(&job).await?;
            emit(on_progress, step, 100);
        }

        let note = self
            .ports
            .notes
            .latest_for_session(id)
            .await?
            .ok_or_else(|| ProcessError::Internal("the finished job has no note".to_owned()))?;
        let action_items = self.ports.notes.list_action_items(&note.id).await?.len();
        let frames_described = self
            .ports
            .frames
            .list_for_session(id)
            .await?
            .iter()
            .filter(|frame| frame.description.is_some())
            .count();
        Ok(ProcessSummary {
            session_id: id.clone(),
            note_id: note.id,
            decisions: note.decisions.len(),
            action_items,
            key_frames: note.frame_refs.len(),
            frames_described,
            frames_skipped: matches!(
                job.state(ProcessingStep::DescribeFrames),
                tan_domain::StepState::Skipped
            ),
        })
    }

    // ----- steps -----------------------------------------------------------------------

    /// The profile assigned to `task`, if any (an assignment whose profile is gone counts as
    /// none).
    async fn assigned(&self, task: TaskKind) -> ProcessResult<Option<ProviderProfile>> {
        let Some(assignment) = self.ports.assignments.get(task).await? else {
            return Ok(None);
        };
        Ok(self.ports.profiles.get(&assignment.profile_id).await?)
    }

    /// Refuses a transcription profile that does not exist or cannot transcribe audio.
    async fn check_transcription_profile(
        &self,
        profile_id: &ProviderProfileId,
    ) -> ProcessResult<()> {
        let profile = self
            .ports
            .profiles
            .get(profile_id)
            .await?
            .ok_or(ProcessError::UnknownTranscriptionProfile)?;
        if Capabilities::for_profile(&profile).transcription {
            Ok(())
        } else {
            Err(ProcessError::Transcribe(TranscribeError::Provider(
                AiError::Unsupported(format!("{} cannot transcribe audio", profile.kind)),
            )))
        }
    }

    async fn step_transcribe(
        &self,
        session: &Session,
        profile: Option<ProviderProfileId>,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<StepEnd> {
        let id = &session.id;
        // The retention policy may have deleted the audio after an earlier run. The transcript
        // is still there, so it stands.
        let tracks = self.ports.sessions.list_audio_tracks(id).await?;
        let audio_available = tracks
            .iter()
            .any(|track| self.ports.files.audio_exists(id, &track.path));
        if !audio_available
            && !self
                .ports
                .transcripts
                .list_for_session(id)
                .await?
                .is_empty()
        {
            return Ok(StepEnd::Done);
        }

        let announce = |progress: crate::transcription::TranscribeProgress| {
            let total = progress.tracks_total.max(1);
            let percent = (progress.tracks_done * 100 / total).min(100);
            emit(
                on_progress,
                ProcessingStep::Transcribe,
                u8::try_from(percent).unwrap_or(100),
            );
        };
        // The profile the job was started with. When it was deleted since (a resumed run), the
        // general assignment applies instead of a run that can never succeed.
        let profile = match profile {
            Some(profile_id) => self
                .ports
                .profiles
                .get(&profile_id)
                .await?
                .map(|existing| existing.id),
            None => None,
        };
        self.ports
            .transcriber
            .transcribe(id, profile.as_ref(), &announce)
            .await?;
        Ok(StepEnd::Done)
    }

    async fn step_describe(
        &self,
        session: &Session,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<StepEnd> {
        let id = &session.id;
        let frames = self.ports.frames.list_for_session(id).await?;
        if frames.is_empty() {
            return Ok(StepEnd::Done);
        }
        let Some(profile) = self
            .assigned(TaskKind::Vision)
            .await?
            .filter(|profile| Capabilities::for_profile(profile).vision)
        else {
            return Ok(StepEnd::Skipped);
        };
        let model = self.ports.factory.vision(&profile).await?;
        let transcript = self.ports.transcripts.list_for_session(id).await?;
        let announce = |percent: u8| emit(on_progress, ProcessingStep::DescribeFrames, percent);
        describe_frames(
            &model,
            &*self.ports.frames,
            &*self.ports.files,
            &*self.ports.ids,
            session,
            &frames,
            &transcript,
            &announce,
        )
        .await?;
        Ok(StepEnd::Done)
    }

    async fn step_synthesize(
        &self,
        session: &Session,
        on_progress: &(dyn Fn(ProcessProgress) + Send + Sync),
    ) -> ProcessResult<StepEnd> {
        let id = &session.id;
        let profile = self
            .assigned(TaskKind::Summary)
            .await?
            .ok_or(ProcessError::NoChatModel)?;
        let model = self.ports.factory.chat(&profile).await?;
        let transcript = self.ports.transcripts.list_for_session(id).await?;
        let described: Vec<_> = self
            .ports
            .frames
            .list_for_session(id)
            .await?
            .into_iter()
            .filter(|frame| frame.description.is_some())
            .collect();

        let announce = |percent: u8| emit(on_progress, ProcessingStep::Synthesize, percent);
        let draft = synthesize(
            &model,
            (self.budget_for)(&profile),
            &SynthesisInput {
                session,
                transcript: &transcript,
                frames: &described,
            },
            &*self.ports.ids,
            &announce,
        )
        .await?;

        let note = Note {
            id: NoteId::new(self.ports.ids.new_id()),
            session_id: id.clone(),
            language: session.note_language.clone(),
            summary: draft.summary,
            decisions: draft.decisions,
            open_questions: draft.open_questions,
            frame_refs: draft.key_frames,
            created_at: self.ports.clock.now(),
        };
        let action_items: Vec<ActionItem> = draft
            .action_items
            .into_iter()
            .map(|item| ActionItem {
                id: ActionItemId::new(self.ports.ids.new_id()),
                note_id: note.id.clone(),
                task: item.task,
                owner: item.owner,
                due: item.due,
                done: false,
            })
            .collect();

        // The new note replaces every earlier one in a single transaction (a crash between two
        // runs may have left strays, and each one still sits in the search index), so the
        // session is never without a note.
        self.ports
            .notes
            .replace_for_session(&note, &action_items)
            .await?;
        Ok(StepEnd::Done)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Wake};

    use super::*;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_pending_future_is_aborted_when_cancel_wakes_it() {
        let state = CancelState::default();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut cx = Context::from_waker(&waker);

        let mut future = pin!(abortable(&state, std::future::pending::<u8>()));
        assert!(future.as_mut().poll(&mut cx).is_pending());
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        state.cancel();
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            1,
            "cancel must wake the task"
        );
        assert_eq!(future.as_mut().poll(&mut cx), Poll::Ready(None));
    }

    #[test]
    fn a_finished_future_yields_its_value() {
        let state = CancelState::default();
        let result = futures_executor_free_block_on(abortable(&state, async { 7 }));
        assert_eq!(result, Some(7));
    }

    #[test]
    fn a_future_cancelled_before_the_first_poll_never_runs() {
        let state = CancelState::default();
        state.cancel();
        let result = futures_executor_free_block_on(abortable(&state, async { 7 }));
        assert_eq!(result, None);
    }

    /// Polls a future that is ready on its first poll (no executor needed).
    fn futures_executor_free_block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(CountingWaker(AtomicUsize::new(0))));
        let mut cx = Context::from_waker(&waker);
        match pin!(future).as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the future was expected to be ready"),
        }
    }

    #[test]
    fn progress_is_weighted_by_step() {
        let seen = Mutex::new(Vec::new());
        let record = |p: ProcessProgress| seen.lock().unwrap().push((p.step, p.percent));
        emit(&record, ProcessingStep::Transcribe, 50);
        emit(&record, ProcessingStep::DescribeFrames, 100);
        emit(&record, ProcessingStep::Synthesize, 0);
        emit(&record, ProcessingStep::Synthesize, 100);
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen,
            [
                (ProcessingStep::Transcribe, 25),
                (ProcessingStep::DescribeFrames, 70),
                (ProcessingStep::Synthesize, 70),
                (ProcessingStep::Synthesize, 100),
            ]
        );
    }
}
