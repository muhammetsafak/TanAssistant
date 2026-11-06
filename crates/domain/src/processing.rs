//! The processing pipeline's state model (ADR-004, ADR-006).
//!
//! A [`ProcessingJob`] records how far the pipeline got for one session: three ordered steps,
//! each with a [`StepState`]. The job is what makes processing resumable: a step that is
//! already finished is never run again, and a step that failed or was interrupted is picked up
//! where the job stopped.

use std::error::Error;
use std::fmt;

use crate::ids::{ProviderProfileId, SessionId};
use crate::EpochMs;

string_enum! {
    /// One stage of the pipeline, in execution order (`ProcessingStep::ALL`).
    #[non_exhaustive]
    pub enum ProcessingStep {
        /// Speech-to-text of the audio tracks.
        Transcribe => "transcribe",
        /// Vision-model descriptions of the screen frames.
        DescribeFrames => "describe_frames",
        /// The structured note: summary, decisions, action items, open questions.
        Synthesize => "synthesize",
    }
}

/// Where one step stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    /// Not started.
    Pending,
    /// Running right now (or the process died while it ran).
    Running,
    /// Finished successfully.
    Done,
    /// Deliberately not run (for example no vision model is assigned); counts as finished.
    Skipped,
    /// Stopped with an error; processing can be retried.
    Failed {
        /// Why it failed (safe to show to the user).
        error: String,
    },
}

impl StepState {
    /// Stable textual tag used for persistence (the error text is stored separately).
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Failed { .. } => "failed",
        }
    }

    /// Rebuilds a state from its tag and the stored error text.
    pub fn from_parts(tag: &str, error: Option<String>) -> Result<Self, ProcessingError> {
        match tag {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "skipped" => Ok(Self::Skipped),
            "failed" => Ok(Self::Failed {
                error: error.unwrap_or_default(),
            }),
            other => Err(ProcessingError::UnknownState(other.to_owned())),
        }
    }

    /// Whether the step needs no further work (`Done` or `Skipped`).
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Skipped)
    }
}

/// A job rule was violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessingError {
    /// The step cannot move to the requested state from where it is.
    InvalidTransition {
        /// The step.
        step: ProcessingStep,
        /// Its current state tag.
        from: &'static str,
        /// The requested state tag.
        to: &'static str,
    },
    /// A step was started before the one before it finished.
    PreviousStepUnfinished(ProcessingStep),
    /// A stored job does not list every step exactly once.
    IncompleteSteps,
    /// A stored state tag is not recognised.
    UnknownState(String),
}

impl fmt::Display for ProcessingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { step, from, to } => {
                write!(f, "step {step} cannot go from {from} to {to}")
            }
            Self::PreviousStepUnfinished(step) => {
                write!(
                    f,
                    "step {step} cannot start before the step before it is finished"
                )
            }
            Self::IncompleteSteps => f.write_str("a job must list every step exactly once"),
            Self::UnknownState(tag) => write!(f, "unknown step state: {tag:?}"),
        }
    }
}

impl Error for ProcessingError {}

/// The persisted shape of a [`ProcessingJob`], used by adapters via [`ProcessingJob::restore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingJobRecord {
    /// The session being processed.
    pub session_id: SessionId,
    /// State of every step, in any order.
    pub steps: Vec<(ProcessingStep, StepState)>,
    /// When the job was created.
    pub created_at: EpochMs,
    /// When a step last changed.
    pub updated_at: EpochMs,
    /// When the last step finished; `None` while unfinished.
    pub finished_at: Option<EpochMs>,
    /// The transcription profile chosen for this job only; `None`: the general assignment.
    pub transcription_profile_id: Option<ProviderProfileId>,
}

/// Progress of the processing pipeline for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingJob {
    session_id: SessionId,
    steps: Vec<(ProcessingStep, StepState)>,
    created_at: EpochMs,
    updated_at: EpochMs,
    finished_at: Option<EpochMs>,
    transcription_profile: Option<ProviderProfileId>,
}

impl ProcessingJob {
    /// A job with every step `Pending`.
    #[must_use]
    pub fn new(session_id: SessionId, now: EpochMs) -> Self {
        Self {
            session_id,
            steps: ProcessingStep::ALL
                .iter()
                .map(|&step| (step, StepState::Pending))
                .collect(),
            created_at: now,
            updated_at: now,
            finished_at: None,
            transcription_profile: None,
        }
    }

    /// Rebuilds a job from stored data; steps are put in execution order.
    pub fn restore(record: ProcessingJobRecord) -> Result<Self, ProcessingError> {
        let mut steps = Vec::with_capacity(ProcessingStep::ALL.len());
        for &step in ProcessingStep::ALL {
            let mut matching = record.steps.iter().filter(|(s, _)| *s == step);
            match (matching.next(), matching.next()) {
                (Some((_, state)), None) => steps.push((step, state.clone())),
                _ => return Err(ProcessingError::IncompleteSteps),
            }
        }
        if record.steps.len() != steps.len() {
            return Err(ProcessingError::IncompleteSteps);
        }
        Ok(Self {
            session_id: record.session_id,
            steps,
            created_at: record.created_at,
            updated_at: record.updated_at,
            finished_at: record.finished_at,
            transcription_profile: record.transcription_profile_id,
        })
    }

    /// The transcription profile chosen for this job only (the general assignment is not
    /// touched). It is kept in the job so that resuming the job after a failure transcribes
    /// with the same profile; a new job starts without one.
    #[must_use]
    pub fn transcription_profile(&self) -> Option<&ProviderProfileId> {
        self.transcription_profile.as_ref()
    }

    /// Chooses (or clears) the transcription profile of this job.
    pub fn set_transcription_profile(&mut self, profile: Option<ProviderProfileId>) {
        self.transcription_profile = profile;
    }

    /// The session being processed.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Every step with its state, in execution order.
    #[must_use]
    pub fn steps(&self) -> &[(ProcessingStep, StepState)] {
        &self.steps
    }

    /// The state of one step.
    #[must_use]
    pub fn state(&self, step: ProcessingStep) -> &StepState {
        // `new` and `restore` guarantee every step is present.
        self.steps
            .iter()
            .find(|(s, _)| *s == step)
            .map_or(&StepState::Pending, |(_, state)| state)
    }

    /// When the job was created.
    #[must_use]
    pub fn created_at(&self) -> EpochMs {
        self.created_at
    }

    /// When a step last changed.
    #[must_use]
    pub fn updated_at(&self) -> EpochMs {
        self.updated_at
    }

    /// When the last step finished; `None` while the job is unfinished.
    #[must_use]
    pub fn finished_at(&self) -> Option<EpochMs> {
        self.finished_at
    }

    /// Whether every step is finished.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.steps.iter().all(|(_, state)| state.is_finished())
    }

    /// The first step that still has work to do.
    #[must_use]
    pub fn next_step(&self) -> Option<ProcessingStep> {
        self.steps
            .iter()
            .find(|(_, state)| !state.is_finished())
            .map(|(step, _)| *step)
    }

    /// The step that failed, with its error, if any.
    #[must_use]
    pub fn failure(&self) -> Option<(ProcessingStep, &str)> {
        self.steps.iter().find_map(|(step, state)| match state {
            StepState::Failed { error } => Some((*step, error.as_str())),
            _ => None,
        })
    }

    fn set(&mut self, step: ProcessingStep, state: StepState, now: EpochMs) {
        if let Some(entry) = self.steps.iter_mut().find(|(s, _)| *s == step) {
            entry.1 = state;
        }
        self.updated_at = now;
        self.finished_at = self.is_complete().then_some(now);
    }

    /// `Pending -> Running`. Every earlier step must be finished.
    pub fn start_step(
        &mut self,
        step: ProcessingStep,
        now: EpochMs,
    ) -> Result<(), ProcessingError> {
        let state = self.state(step);
        if *state != StepState::Pending {
            return Err(ProcessingError::InvalidTransition {
                step,
                from: state.tag(),
                to: "running",
            });
        }
        let unfinished_before = self
            .steps
            .iter()
            .take_while(|(s, _)| *s != step)
            .any(|(_, state)| !state.is_finished());
        if unfinished_before {
            return Err(ProcessingError::PreviousStepUnfinished(step));
        }
        self.set(step, StepState::Running, now);
        Ok(())
    }

    fn finish_running(
        &mut self,
        step: ProcessingStep,
        to: StepState,
        now: EpochMs,
    ) -> Result<(), ProcessingError> {
        let state = self.state(step);
        if *state != StepState::Running {
            return Err(ProcessingError::InvalidTransition {
                step,
                from: state.tag(),
                to: to.tag(),
            });
        }
        self.set(step, to, now);
        Ok(())
    }

    /// `Running -> Done`.
    pub fn complete_step(
        &mut self,
        step: ProcessingStep,
        now: EpochMs,
    ) -> Result<(), ProcessingError> {
        self.finish_running(step, StepState::Done, now)
    }

    /// `Running -> Skipped`.
    pub fn skip_step(&mut self, step: ProcessingStep, now: EpochMs) -> Result<(), ProcessingError> {
        self.finish_running(step, StepState::Skipped, now)
    }

    /// `Running -> Failed`.
    pub fn fail_step(
        &mut self,
        step: ProcessingStep,
        error: impl Into<String>,
        now: EpochMs,
    ) -> Result<(), ProcessingError> {
        self.finish_running(
            step,
            StepState::Failed {
                error: error.into(),
            },
            now,
        )
    }

    /// `Running -> Pending` for every running step: the run was cancelled, or the process died
    /// and the step has to start over.
    pub fn interrupt(&mut self, now: EpochMs) {
        let mut changed = false;
        for (_, state) in &mut self.steps {
            if *state == StepState::Running {
                *state = StepState::Pending;
                changed = true;
            }
        }
        if changed {
            self.updated_at = now;
        }
    }

    /// Prepares a resumed run: the leading run of `Done` steps is kept; the first step that is
    /// not `Done` (failed, interrupted, or skipped — a vision model may have been assigned
    /// since) and **every step after it** start over. A later step consumes what the earlier
    /// ones produce, so once one runs again, the results built on its old output are stale
    /// even if they finished (for example the note after frames were described anew).
    pub fn prepare_retry(&mut self, now: EpochMs) {
        let mut invalidated = false;
        for (_, state) in &mut self.steps {
            invalidated |= !matches!(state, StepState::Done);
            if invalidated {
                *state = StepState::Pending;
            }
        }
        self.updated_at = now;
        self.finished_at = self.is_complete().then_some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessingError, ProcessingJob, ProcessingJobRecord, ProcessingStep, StepState};
    use crate::ids::{ProviderProfileId, SessionId};
    use crate::EpochMs;

    fn job() -> ProcessingJob {
        ProcessingJob::new(SessionId::new("s1"), EpochMs::new(10))
    }

    fn at(ms: i64) -> EpochMs {
        EpochMs::new(ms)
    }

    #[test]
    fn step_text_round_trips() {
        for &step in ProcessingStep::ALL {
            assert_eq!(step.as_str().parse::<ProcessingStep>().unwrap(), step);
        }
    }

    #[test]
    fn state_tags_round_trip() {
        let failed = StepState::Failed {
            error: "boom".to_owned(),
        };
        for state in [
            StepState::Pending,
            StepState::Running,
            StepState::Done,
            StepState::Skipped,
            failed,
        ] {
            let error = match &state {
                StepState::Failed { error } => Some(error.clone()),
                _ => None,
            };
            assert_eq!(StepState::from_parts(state.tag(), error).unwrap(), state);
        }
        assert!(StepState::from_parts("nope", None).is_err());
    }

    #[test]
    fn steps_run_in_order_and_finish_the_job() {
        let mut job = job();
        assert_eq!(job.next_step(), Some(ProcessingStep::Transcribe));
        assert_eq!(
            job.start_step(ProcessingStep::Synthesize, at(11)),
            Err(ProcessingError::PreviousStepUnfinished(
                ProcessingStep::Synthesize
            ))
        );

        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        job.complete_step(ProcessingStep::Transcribe, at(12))
            .unwrap();
        job.start_step(ProcessingStep::DescribeFrames, at(13))
            .unwrap();
        job.skip_step(ProcessingStep::DescribeFrames, at(14))
            .unwrap();
        assert!(!job.is_complete());
        assert_eq!(job.next_step(), Some(ProcessingStep::Synthesize));

        job.start_step(ProcessingStep::Synthesize, at(15)).unwrap();
        job.complete_step(ProcessingStep::Synthesize, at(16))
            .unwrap();
        assert!(job.is_complete());
        assert_eq!(job.finished_at(), Some(at(16)));
        assert_eq!(job.next_step(), None);
    }

    #[test]
    fn a_step_cannot_finish_without_running() {
        let mut job = job();
        assert!(matches!(
            job.complete_step(ProcessingStep::Transcribe, at(11)),
            Err(ProcessingError::InvalidTransition { .. })
        ));
        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        assert!(matches!(
            job.start_step(ProcessingStep::Transcribe, at(12)),
            Err(ProcessingError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn retry_keeps_done_steps_and_restarts_the_rest() {
        let mut job = job();
        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        job.complete_step(ProcessingStep::Transcribe, at(12))
            .unwrap();
        job.start_step(ProcessingStep::DescribeFrames, at(13))
            .unwrap();
        job.skip_step(ProcessingStep::DescribeFrames, at(14))
            .unwrap();
        job.start_step(ProcessingStep::Synthesize, at(15)).unwrap();
        job.fail_step(ProcessingStep::Synthesize, "boom", at(16))
            .unwrap();
        assert_eq!(job.failure(), Some((ProcessingStep::Synthesize, "boom")));

        job.prepare_retry(at(20));
        assert_eq!(*job.state(ProcessingStep::Transcribe), StepState::Done);
        assert_eq!(
            *job.state(ProcessingStep::DescribeFrames),
            StepState::Pending
        );
        assert_eq!(*job.state(ProcessingStep::Synthesize), StepState::Pending);
        assert_eq!(job.failure(), None);
        assert_eq!(job.finished_at(), None);
    }

    #[test]
    fn retry_restarts_the_steps_after_a_reopened_skipped_step_even_when_they_are_done() {
        // Processed without a vision model: the frame step was skipped, the note was written.
        let mut job = job();
        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        job.complete_step(ProcessingStep::Transcribe, at(12))
            .unwrap();
        job.start_step(ProcessingStep::DescribeFrames, at(13))
            .unwrap();
        job.skip_step(ProcessingStep::DescribeFrames, at(14))
            .unwrap();
        job.start_step(ProcessingStep::Synthesize, at(15)).unwrap();
        job.complete_step(ProcessingStep::Synthesize, at(16))
            .unwrap();
        assert!(job.is_complete());

        job.prepare_retry(at(20));
        assert_eq!(*job.state(ProcessingStep::Transcribe), StepState::Done);
        assert_eq!(
            *job.state(ProcessingStep::DescribeFrames),
            StepState::Pending
        );
        assert_eq!(
            *job.state(ProcessingStep::Synthesize),
            StepState::Pending,
            "the note depends on the frame descriptions"
        );
        assert_eq!(job.next_step(), Some(ProcessingStep::DescribeFrames));
        assert_eq!(job.finished_at(), None);
    }

    #[test]
    fn retrying_a_fully_done_job_changes_nothing() {
        let mut job = job();
        for step in ProcessingStep::ALL {
            job.start_step(*step, at(11)).unwrap();
            job.complete_step(*step, at(12)).unwrap();
        }
        job.prepare_retry(at(20));
        assert!(job.is_complete());
        assert_eq!(job.next_step(), None);
    }

    #[test]
    fn interrupt_returns_running_steps_to_pending() {
        let mut job = job();
        job.start_step(ProcessingStep::Transcribe, at(11)).unwrap();
        job.interrupt(at(12));
        assert_eq!(*job.state(ProcessingStep::Transcribe), StepState::Pending);
        assert_eq!(job.updated_at(), at(12));
    }

    #[test]
    fn the_transcription_profile_is_per_job_and_survives_a_restore() {
        let mut job = job();
        assert_eq!(job.transcription_profile(), None);
        job.set_transcription_profile(Some(ProviderProfileId::new("cloud")));
        assert_eq!(
            job.transcription_profile(),
            Some(&ProviderProfileId::new("cloud"))
        );
        let restored = ProcessingJob::restore(ProcessingJobRecord {
            session_id: SessionId::new("s1"),
            steps: job.steps().to_vec(),
            created_at: job.created_at(),
            updated_at: job.updated_at(),
            finished_at: job.finished_at(),
            transcription_profile_id: job.transcription_profile().cloned(),
        })
        .unwrap();
        assert_eq!(restored, job);
        job.set_transcription_profile(None);
        assert_eq!(job.transcription_profile(), None);
    }

    #[test]
    fn restore_requires_every_step_exactly_once() {
        let record = |steps: Vec<(ProcessingStep, StepState)>| ProcessingJobRecord {
            session_id: SessionId::new("s1"),
            steps,
            created_at: at(1),
            updated_at: at(2),
            finished_at: None,
            transcription_profile_id: None,
        };
        assert_eq!(
            ProcessingJob::restore(record(vec![(ProcessingStep::Transcribe, StepState::Done)])),
            Err(ProcessingError::IncompleteSteps)
        );
        let doubled = vec![
            (ProcessingStep::Transcribe, StepState::Done),
            (ProcessingStep::Transcribe, StepState::Pending),
            (ProcessingStep::DescribeFrames, StepState::Pending),
            (ProcessingStep::Synthesize, StepState::Pending),
        ];
        assert_eq!(
            ProcessingJob::restore(record(doubled)),
            Err(ProcessingError::IncompleteSteps)
        );
        let shuffled = vec![
            (ProcessingStep::Synthesize, StepState::Pending),
            (ProcessingStep::Transcribe, StepState::Done),
            (ProcessingStep::DescribeFrames, StepState::Pending),
        ];
        let restored = ProcessingJob::restore(record(shuffled)).unwrap();
        assert_eq!(restored.steps()[0].0, ProcessingStep::Transcribe);
        assert_eq!(restored.next_step(), Some(ProcessingStep::DescribeFrames));
    }
}
