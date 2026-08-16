//! Timer-free auto-resume policy.

use coducktor_contract::runs::{RunRecord, RunStatus};

use super::{AccountHolds, Runner, account_held_for};

pub use super::{AutoResumeReport, MAX_AUTO_RESUME_ATTEMPTS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoResumeDecision {
    Scheduled,
    Due,
    Held,
    Stale,
}

pub fn decision(
    run: &RunRecord,
    now: &str,
    holds: &AccountHolds,
    fallback_runner: Runner,
) -> Option<AutoResumeDecision> {
    let deadline = run.auto_resume_at.as_deref()?;
    let valid = run.status == RunStatus::Failed
        && !run.archived
        && super::is_zod_datetime(deadline)
        && run.steps.iter().any(|step| step.session_id.is_some())
        && run.auto_resume_attempts.unwrap_or(0.0) < MAX_AUTO_RESUME_ATTEMPTS;
    if !valid {
        return Some(AutoResumeDecision::Stale);
    }
    if deadline > now {
        Some(AutoResumeDecision::Scheduled)
    } else if account_held_for(run, holds, fallback_runner) {
        Some(AutoResumeDecision::Held)
    } else {
        Some(AutoResumeDecision::Due)
    }
}

pub fn retry_allowed(attempts: f64) -> bool {
    attempts.is_finite() && (0.0..MAX_AUTO_RESUME_ATTEMPTS).contains(&attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(status: RunStatus, deadline: Option<&str>, attempts: Option<f64>) -> RunRecord {
        let step = serde_json::from_value(json!({
            "id": "agent",
            "name": "Agent",
            "kind": "agent",
            "status": "failed",
            "iterations": 1,
            "tokensUsed": 0,
            "sessionId": "session"
        }))
        .expect("test step is a valid contract value");
        RunRecord {
            status,
            auto_resume_at: deadline.map(str::to_owned),
            auto_resume_attempts: attempts,
            steps: vec![step],
            ..RunRecord::default()
        }
    }

    #[test]
    fn deadline_policy_distinguishes_future_due_and_stale() {
        let holds = AccountHolds::default();
        assert_eq!(
            decision(
                &run(RunStatus::Failed, Some("2099-01-01T00:00:00.000Z"), None),
                "2026-01-01T00:00:00.000Z",
                &holds,
                Runner::Claude,
            ),
            Some(AutoResumeDecision::Scheduled)
        );
        assert_eq!(
            decision(
                &run(RunStatus::Failed, Some("2020-01-01T00:00:00.000Z"), None),
                "2026-01-01T00:00:00.000Z",
                &holds,
                Runner::Claude,
            ),
            Some(AutoResumeDecision::Due)
        );
        assert_eq!(
            decision(
                &run(RunStatus::Done, Some("2099-01-01T00:00:00.000Z"), None),
                "2026-01-01T00:00:00.000Z",
                &holds,
                Runner::Claude,
            ),
            Some(AutoResumeDecision::Stale)
        );
    }

    #[test]
    fn retry_cap_is_explicit() {
        assert!(retry_allowed(0.0));
        assert!(!retry_allowed(MAX_AUTO_RESUME_ATTEMPTS));
    }
}
