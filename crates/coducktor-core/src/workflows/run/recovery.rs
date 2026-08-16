//! Pure restart-recovery classification.

use coducktor_contract::runs::{RunRecord, RunStatus, StepStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Requeue,
    SettleWaiting,
    ResumeInterrupted,
    FailMissingWorkflow,
    Ignore,
}

pub fn action(run: &RunRecord, workflow_available: bool) -> RecoveryAction {
    match run.status {
        RunStatus::Queued => {
            if workflow_available {
                RecoveryAction::Requeue
            } else {
                RecoveryAction::FailMissingWorkflow
            }
        }
        RunStatus::Waiting => RecoveryAction::SettleWaiting,
        RunStatus::Running => {
            if run.steps.iter().any(|step| {
                step.session_id.is_some()
                    && matches!(step.status, StepStatus::Running | StepStatus::Waiting)
            }) {
                RecoveryAction::ResumeInterrupted
            } else {
                RecoveryAction::Ignore
            }
        }
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review => {
            RecoveryAction::Ignore
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live(status: RunStatus) -> RunRecord {
        RunRecord {
            status,
            steps: vec![
                serde_json::from_value(json!({
                    "id": "step",
                    "name": "Step",
                    "kind": "agent",
                    "status": "running",
                    "iterations": 1,
                    "tokensUsed": 0,
                    "sessionId": "session"
                }))
                .expect("test step is valid"),
            ],
            ..RunRecord::default()
        }
    }

    #[test]
    fn recovery_classification_covers_live_durable_states() {
        assert_eq!(
            action(&live(RunStatus::Queued), true),
            RecoveryAction::Requeue
        );
        assert_eq!(
            action(&live(RunStatus::Queued), false),
            RecoveryAction::FailMissingWorkflow
        );
        assert_eq!(
            action(&live(RunStatus::Waiting), true),
            RecoveryAction::SettleWaiting
        );
        assert_eq!(
            action(&live(RunStatus::Running), true),
            RecoveryAction::ResumeInterrupted
        );
    }
}
