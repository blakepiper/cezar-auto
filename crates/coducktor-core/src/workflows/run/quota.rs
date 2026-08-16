//! Quota routing policy that stays independent of provider clients.

use coducktor_contract::runs::{RunRecord, RunStatus};

use super::{AccountHolds, Runner, account_held_for};

pub const MAX_AUTO_RESUME_ATTEMPTS: f64 = 12.0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaReconciliation {
    pub holds: AccountHolds,
    pub scheduled: Vec<String>,
    pub due: Vec<String>,
    pub held: Vec<String>,
    pub blocked_queue: Vec<String>,
    pub stale: Vec<String>,
}

pub fn reconcile_quota(
    runs: &[RunRecord],
    now: &str,
    fallback_runner: Runner,
) -> QuotaReconciliation {
    let holds = super::derive_account_holds(runs, now);
    let mut reconciliation = QuotaReconciliation {
        holds,
        ..QuotaReconciliation::default()
    };
    for run in runs {
        if run.status == RunStatus::Queued
            && account_held_for(run, &reconciliation.holds, fallback_runner)
        {
            reconciliation.blocked_queue.push(run.id.clone());
        }
        let Some(deadline) = run.auto_resume_at.as_deref() else {
            continue;
        };
        let valid = run.status == RunStatus::Failed
            && !run.archived
            && super::is_zod_datetime(deadline)
            && run.steps.iter().any(|step| step.session_id.is_some())
            && run.auto_resume_attempts.unwrap_or(0.0) < MAX_AUTO_RESUME_ATTEMPTS;
        if !valid {
            reconciliation.stale.push(run.id.clone());
            continue;
        }
        if deadline > now {
            reconciliation.scheduled.push(run.id.clone());
        } else if account_held_for(run, &reconciliation.holds, fallback_runner) {
            reconciliation.held.push(run.id.clone());
        } else {
            reconciliation.due.push(run.id.clone());
        }
    }
    for ids in [
        &mut reconciliation.scheduled,
        &mut reconciliation.due,
        &mut reconciliation.held,
        &mut reconciliation.blocked_queue,
        &mut reconciliation.stale,
    ] {
        ids.sort();
    }
    reconciliation
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Deadline,
    InFlight,
    None,
}

pub fn hold_kind(run: &RunRecord, holds: &AccountHolds, fallback_runner: Runner) -> HoldKind {
    let key = super::run_account_key(run, fallback_runner);
    if holds.deadline.contains(&key) {
        HoldKind::Deadline
    } else if holds.in_flight.contains(&key) && !super::resume_in_flight(run) {
        HoldKind::InFlight
    } else {
        HoldKind::None
    }
}

pub fn can_start_fresh(run: &RunRecord, holds: &AccountHolds, fallback_runner: Runner) -> bool {
    run.status == RunStatus::Queued && !account_held_for(run, holds, fallback_runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn queued() -> RunRecord {
        RunRecord {
            runner: Some(Runner::Claude),
            status: RunStatus::Queued,
            ..RunRecord::default()
        }
    }

    #[test]
    fn deadline_blocks_fresh_work_but_in_flight_resume_does_not() {
        let run = queued();
        let key = super::super::run_account_key(&run, Runner::Claude);
        let deadline = AccountHolds {
            deadline: [key.clone()].into_iter().collect(),
            in_flight: BTreeSet::new(),
        };
        assert_eq!(
            hold_kind(&run, &deadline, Runner::Claude),
            HoldKind::Deadline
        );
        assert!(!can_start_fresh(&run, &deadline, Runner::Claude));

        let in_flight = AccountHolds {
            deadline: BTreeSet::new(),
            in_flight: [key].into_iter().collect(),
        };
        assert_eq!(
            hold_kind(&run, &in_flight, Runner::Claude),
            HoldKind::InFlight
        );
        assert!(!can_start_fresh(&run, &in_flight, Runner::Claude));
    }
}
