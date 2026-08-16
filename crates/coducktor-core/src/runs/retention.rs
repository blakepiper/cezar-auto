//! Count-based worktree retention (#483). Mirrors the PURE half of
//! `packages/cezar/src/runs/retention.ts`: which finished worktrees are over budget and
//! should have their *directory* reclaimed (the `duck/<id8>` branch stays, so the work is
//! always recoverable via `git worktree add`).
//!
//! `retention.ts`'s I/O enforcer (`reclaimWorktrees`) and re-materializer
//! (`rematerializeReclaimedWorktree`) are deliberately NOT ported here: both call into
//! `git-worktree.ts` (`createWorktree`/`removeWorktree`), which does not exist in Rust yet —
//! that is `cezar-core::git`, landing at **B3**. Porting the enforcer against a git layer
//! that doesn't exist would mean re-deriving it there anyway; the selector below is exactly
//! the boundary `retention.ts` itself draws between "pure and unit-testable" and "thin I/O",
//! so B3 wires this selector straight into its own `reclaim_worktrees`.

use coducktor_contract::RunStatus;
use coducktor_contract::runs::RunRecord;

/// The "finished" status set — mirrors `RunStore.archiveFinished`. A run at the `review`
/// gate is deliberately excluded: it still needs its worktree to render the diff and open a
/// draft PR, so reclaiming it would break the gate.
fn is_finished(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
    )
}

/// Recency key for retention ordering: when a run finished, falling back to when it was
/// created (a finished run should always have `finishedAt`, but old records may not).
fn recency_key(run: &RunRecord) -> &str {
    run.finished_at.as_deref().unwrap_or(&run.created_at)
}

/// A run is reclaimable when it is finished, still has a materialized worktree directory,
/// and has not already been reclaimed.
pub fn is_reclaimable(run: &RunRecord) -> bool {
    is_finished(run.status) && run.worktree_path.is_some() && run.worktree_reclaimed_at.is_none()
}

/// Given every run and the keep-count `keep`, the ids of the finished worktrees whose
/// *directory* should be reclaimed: keep the `keep` most-recently-finished reclaimable
/// worktrees, reclaim the rest. `keep == 0` means "unlimited — never auto-reclaim" and
/// returns `[]`. Pure: no I/O, no mutation of the input.
pub fn select_reclaimable_worktrees(runs: &[RunRecord], keep: u64) -> Vec<String> {
    if keep == 0 {
        return Vec::new();
    }
    let mut reclaimable: Vec<&RunRecord> = runs.iter().filter(|r| is_reclaimable(r)).collect();
    reclaimable.sort_by(|a, b| recency_key(b).cmp(recency_key(a)));
    reclaimable
        .into_iter()
        .skip(keep as usize)
        .map(|r| r.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        id: &str,
        status: RunStatus,
        worktree_path: Option<&str>,
        created_at: &str,
        finished_at: Option<&str>,
        worktree_reclaimed_at: Option<&str>,
    ) -> RunRecord {
        RunRecord {
            id: id.into(),
            status,
            created_at: created_at.into(),
            finished_at: finished_at.map(str::to_owned),
            worktree_path: worktree_path.map(str::to_owned),
            worktree_reclaimed_at: worktree_reclaimed_at.map(str::to_owned),
            ..Default::default()
        }
    }

    fn done(id: &str, finished_at: &str) -> RunRecord {
        run(
            id,
            RunStatus::Done,
            Some(&format!("/wt/{id}")),
            "2026-01-01T00:00:00.000Z",
            Some(finished_at),
            None,
        )
    }

    #[test]
    fn keeps_the_newest_n_and_reclaims_the_older_ones() {
        let runs = vec![
            done("a", "2026-07-01T00:00:00Z"),
            done("b", "2026-07-02T00:00:00Z"),
            run(
                "c",
                RunStatus::Failed,
                Some("/wt/c"),
                "2026-01-01T00:00:00.000Z",
                Some("2026-07-03T00:00:00Z"),
                None,
            ),
            run(
                "d",
                RunStatus::Cancelled,
                Some("/wt/d"),
                "2026-01-01T00:00:00.000Z",
                Some("2026-07-04T00:00:00Z"),
                None,
            ),
        ];
        let mut reclaimed = select_reclaimable_worktrees(&runs, 2);
        reclaimed.sort();
        assert_eq!(reclaimed, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn orders_by_finished_at_falling_back_to_created_at() {
        let runs = vec![
            run(
                "old",
                RunStatus::Done,
                Some("/wt/old"),
                "2026-06-01T00:00:00Z",
                None,
                None,
            ),
            done("new", "2026-07-09T00:00:00Z"),
        ];
        assert_eq!(select_reclaimable_worktrees(&runs, 1), vec!["old"]);
    }

    #[test]
    fn excludes_review_and_live_runs_from_the_budget_entirely() {
        let runs = vec![
            run(
                "review",
                RunStatus::Review,
                Some("/wt/review"),
                "2026-01-01T00:00:00.000Z",
                Some("2026-07-09T00:00:00Z"),
                None,
            ),
            run(
                "running",
                RunStatus::Running,
                Some("/wt/running"),
                "2026-01-01T00:00:00.000Z",
                None,
                None,
            ),
            done("done1", "2026-07-01T00:00:00Z"),
            done("done2", "2026-07-02T00:00:00Z"),
        ];
        assert_eq!(select_reclaimable_worktrees(&runs, 1), vec!["done1"]);
    }

    #[test]
    fn excludes_runs_with_no_worktree_dir_and_already_reclaimed_runs() {
        let runs = vec![
            run(
                "nodir",
                RunStatus::Done,
                None,
                "2026-01-01T00:00:00.000Z",
                Some("2026-07-01T00:00:00Z"),
                None,
            ),
            run(
                "gone",
                RunStatus::Done,
                Some("/wt/gone"),
                "2026-01-01T00:00:00.000Z",
                Some("2026-07-02T00:00:00Z"),
                Some("2026-07-05T00:00:00Z"),
            ),
            done("live-dir", "2026-07-03T00:00:00Z"),
        ];
        assert_eq!(select_reclaimable_worktrees(&runs, 5), Vec::<String>::new());
    }

    #[test]
    fn treats_keep_zero_as_unlimited() {
        let runs = vec![
            done("a", "2026-07-01T00:00:00Z"),
            done("b", "2026-07-02T00:00:00Z"),
        ];
        assert_eq!(select_reclaimable_worktrees(&runs, 0), Vec::<String>::new());
    }

    #[test]
    fn reclaims_nothing_when_the_count_is_at_or_below_the_limit() {
        let runs = vec![done("a", "2026-07-01T00:00:00Z")];
        assert_eq!(
            select_reclaimable_worktrees(&runs, 10),
            Vec::<String>::new()
        );
    }

    #[test]
    fn is_reclaimable_reflects_the_finished_has_dir_not_yet_reclaimed_rule() {
        assert!(is_reclaimable(&run(
            "x",
            RunStatus::Done,
            Some("/wt/x"),
            "2026-01-01T00:00:00.000Z",
            None,
            None
        )));
        assert!(!is_reclaimable(&run(
            "x",
            RunStatus::Review,
            Some("/wt/x"),
            "2026-01-01T00:00:00.000Z",
            None,
            None
        )));
        assert!(!is_reclaimable(&run(
            "x",
            RunStatus::Done,
            None,
            "2026-01-01T00:00:00.000Z",
            None,
            None
        )));
        assert!(!is_reclaimable(&run(
            "x",
            RunStatus::Done,
            Some("/wt/x"),
            "2026-01-01T00:00:00.000Z",
            None,
            Some("2026-07-05T00:00:00Z")
        )));
    }
}
