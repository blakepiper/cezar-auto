//! Pure lifecycle policy shared by the runtime and its future adapters.

use coducktor_contract::runs::{RunRecord, RunStatus, StepStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Queued,
    Running,
    Waiting,
    Review,
    Terminal,
}

pub fn lifecycle_state(status: RunStatus) -> LifecycleState {
    match status {
        RunStatus::Queued => LifecycleState::Queued,
        RunStatus::Running => LifecycleState::Running,
        RunStatus::Waiting => LifecycleState::Waiting,
        RunStatus::Review => LifecycleState::Review,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => LifecycleState::Terminal,
    }
}

pub fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review
    )
}

pub fn is_live(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Queued | RunStatus::Running | RunStatus::Waiting
    )
}

pub fn unfinished_step_ids(run: &RunRecord) -> Vec<String> {
    run.steps
        .iter()
        .filter(|step| {
            matches!(
                step.status,
                StepStatus::Pending | StepStatus::Running | StepStatus::Waiting
            )
        })
        .map(|step| step.id.clone())
        .collect()
}

pub fn retry_allowed(used: u32, max: u32) -> bool {
    used < max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_states_are_total_and_terminal_review_is_distinct() {
        assert_eq!(lifecycle_state(RunStatus::Queued), LifecycleState::Queued);
        assert_eq!(lifecycle_state(RunStatus::Waiting), LifecycleState::Waiting);
        assert_eq!(lifecycle_state(RunStatus::Review), LifecycleState::Review);
        assert!(is_terminal(RunStatus::Failed));
        assert!(!is_terminal(RunStatus::Running));
    }

    #[test]
    fn retry_policy_is_bounded() {
        assert!(retry_allowed(0, 1));
        assert!(!retry_allowed(1, 1));
    }
}
