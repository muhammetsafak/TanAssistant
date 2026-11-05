//! Which provider profile performs which task.

use std::sync::Arc;

use tan_domain::{ProviderProfileId, TaskAssignment, TaskKind};

use super::{AiUseCaseError, AiUseCaseResult};
use crate::ports::{Capabilities, ProviderProfileRepository, TaskAssignmentRepository};

const DANGLING_PROFILE: &str = "the provider profile no longer exists";

/// Assigns a task to a profile, refusing profiles whose provider lacks the capability.
pub struct SetTaskAssignment<P, A> {
    profiles: Arc<P>,
    assignments: Arc<A>,
}

impl<P: ProviderProfileRepository, A: TaskAssignmentRepository> SetTaskAssignment<P, A> {
    /// Creates the use case.
    #[must_use]
    pub fn new(profiles: Arc<P>, assignments: Arc<A>) -> Self {
        Self {
            profiles,
            assignments,
        }
    }

    /// Assigns (or re-assigns) `task` to the profile.
    pub async fn execute(
        &self,
        task: TaskKind,
        profile_id: &ProviderProfileId,
    ) -> AiUseCaseResult<TaskAssignment> {
        let profile = self
            .profiles
            .get(profile_id)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_PROFILE))?
            .ok_or(AiUseCaseError::NotFound {
                entity: "provider profile",
            })?;
        if !Capabilities::for_profile(&profile).supports(task) {
            return Err(AiUseCaseError::CapabilityMissing {
                task,
                provider: profile.kind,
            });
        }
        let assignment = TaskAssignment {
            task,
            profile_id: profile.id,
        };
        self.assignments
            .set(&assignment)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_PROFILE))?;
        Ok(assignment)
    }
}

/// Reads every task assignment.
pub struct GetTaskAssignments<A> {
    assignments: Arc<A>,
}

impl<A: TaskAssignmentRepository> GetTaskAssignments<A> {
    /// Creates the use case.
    #[must_use]
    pub fn new(assignments: Arc<A>) -> Self {
        Self { assignments }
    }

    /// Runs it.
    pub async fn execute(&self) -> AiUseCaseResult<Vec<TaskAssignment>> {
        self.assignments
            .list()
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_PROFILE))
    }
}

/// Removes a task's assignment. Without a transcription assignment the application uses local
/// whisper (ADR-003).
pub struct ClearTaskAssignment<A> {
    assignments: Arc<A>,
}

impl<A: TaskAssignmentRepository> ClearTaskAssignment<A> {
    /// Creates the use case.
    #[must_use]
    pub fn new(assignments: Arc<A>) -> Self {
        Self { assignments }
    }

    /// Runs it; clearing an unassigned task is not an error.
    pub async fn execute(&self, task: TaskKind) -> AiUseCaseResult<()> {
        self.assignments
            .clear(task)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_PROFILE))
    }
}
