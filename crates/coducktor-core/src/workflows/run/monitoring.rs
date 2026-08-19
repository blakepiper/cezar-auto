//! Timer-free monitoring-wake policy: which parked monitoring sessions are due a check-in.
//!
//! Mirrors [`super::auto_resume`]'s shape — a pure decision function over a durable record and
//! the caller's `now`, so the actual reconciliation driver (`RunManager::due_monitoring_wakes`)
//! stays a thin, testable filter rather than embedding this logic inline.

use coducktor_contract::runs::{RunActivity, RunRecord};

/// A prior turn parked with `activity: Monitoring` and a durable `monitoringWakeAt` deadline;
/// checking `deadline <= now` (lexicographic — both are the same fixed-width UTC ISO-8601
/// spelling) is due for a check-in nudge.
pub fn is_due(run: &RunRecord, now: &str) -> bool {
    run.activity == Some(RunActivity::Monitoring)
        && run
            .monitoring_wake_at
            .as_deref()
            .is_some_and(|deadline| super::is_zod_datetime(deadline) && deadline <= now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(activity: Option<RunActivity>, deadline: Option<&str>) -> RunRecord {
        RunRecord {
            activity,
            monitoring_wake_at: deadline.map(str::to_owned),
            ..RunRecord::default()
        }
    }

    #[test]
    fn only_a_monitoring_run_with_a_past_valid_deadline_is_due() {
        let now = "2026-01-01T00:00:00.000Z";
        assert!(is_due(
            &run(
                Some(RunActivity::Monitoring),
                Some("2020-01-01T00:00:00.000Z")
            ),
            now
        ));
        assert!(
            !is_due(
                &run(
                    Some(RunActivity::Monitoring),
                    Some("2099-01-01T00:00:00.000Z")
                ),
                now
            ),
            "a future deadline is not yet due"
        );
        assert!(
            !is_due(&run(None, Some("2020-01-01T00:00:00.000Z")), now),
            "a non-monitoring run is never due regardless of the field"
        );
        assert!(
            !is_due(&run(Some(RunActivity::Monitoring), None), now),
            "no deadline means never due"
        );
        assert!(
            !is_due(&run(Some(RunActivity::Monitoring), Some("not-a-date")), now),
            "a malformed deadline degrades to never due, not a panic"
        );
    }
}
