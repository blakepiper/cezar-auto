//! Count-based worktree retention (#483). Mirrors `packages/cezar/src/runs/retention.ts`:
//! which finished worktrees are over budget and should have their *directory* reclaimed
//! (the `duck/<id8>` branch stays, so the work is always recoverable via
//! `git worktree add`).
//!
//! [`reclaim_worktrees`] and [`rematerialize_reclaimed_worktree`] are the B3 I/O half,
//! wired against `crate::git::worktree` now that it exists. They deliberately do **not**
//! match `reclaimWorktrees`/`rematerializeReclaimedWorktree`'s TS signatures: those take an
//! injectable `store` (`RetentionStore`/`RematerializeStore`, backed by the real `RunStore`
//! class) and mutate it directly via `store.updateRun(id, { worktreeReclaimedAt })`. No
//! live run store exists in Rust yet — `RunManager` is **B6** — so these functions return
//! what changed (reclaimed run ids + timestamps, or the rematerialized `WorktreeInfo`)
//! instead of persisting it; B6's `RunManager` is what will call these and write the result
//! through `runs::store::write_run_index`. That keeps this step honest about which layer
//! actually exists right now rather than inventing a store trait whose real shape is B6's
//! to decide.

use std::path::Path;

use coducktor_contract::RunStatus;
use coducktor_contract::runs::RunRecord;

use crate::git::worktree;

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

/// Enforce the retention budget: reclaim the *directory* of every over-limit finished
/// worktree (branch kept — `worktree::remove_worktree` is called without a branch arg).
/// Returns `(run_id, reclaimed_at)` for each run actually reclaimed, for the caller to
/// persist and log/SSE.
///
/// Never panics (helper discipline). `remove_worktree` is best-effort and does not report
/// failure, so a run is reported reclaimed only once its directory is confirmed gone — a
/// locked/permission failure leaves it unreported so the next pass retries. Idempotent
/// under races: `remove_worktree` is `--force` + `prune`, so a repeated call is harmless.
pub fn reclaim_worktrees(
    repo_root: &Path,
    runs: &[RunRecord],
    keep: u64,
    now: impl Fn() -> String,
) -> Vec<(String, String)> {
    let by_id = |id: &str| runs.iter().find(|r| r.id == id);
    let mut reclaimed = Vec::new();
    for id in select_reclaimable_worktrees(runs, keep) {
        let Some(run) = by_id(&id) else { continue };
        let Some(worktree_path) = run.worktree_path.as_deref() else {
            continue;
        };
        let path = Path::new(worktree_path);
        worktree::remove_worktree(repo_root, path, None);
        if path.exists() {
            continue; // reclaim failed; retry next pass
        }
        reclaimed.push((id, now()));
    }
    reclaimed
}

/// Re-materialize a reclaimed worktree directory on demand (a run the user comes back to
/// after its directory was reclaimed). `None` when the run has no worktree to restore, was
/// never reclaimed, its directory is already present, or `git worktree add` fails. The
/// caller clears `worktree_reclaimed_at` on `Some`.
pub fn rematerialize_reclaimed_worktree(
    repo_root: &Path,
    run: &RunRecord,
) -> Option<worktree::WorktreeInfo> {
    let worktree_path = run.worktree_path.as_deref()?;
    run.worktree_reclaimed_at.as_deref()?;
    if Path::new(worktree_path).exists() {
        return None;
    }
    worktree::create_worktree(
        repo_root,
        &run.id,
        run.base_branch.as_deref().unwrap_or("HEAD"),
    )
    .ok()
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

    /// A real repo with a real `duck/<id8>` worktree — `reclaim_worktrees`/
    /// `rematerialize_reclaimed_worktree` are thin wrappers over `git::worktree`, but the
    /// wrapping (which run's directory gets removed, whether the report is honest about
    /// what actually happened on disk) is exactly what these tests check.
    fn fixture_repo_with_worktree(run_id: &str) -> (tempfile::TempDir, worktree::WorktreeInfo) {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .current_dir(root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-q",
            "-m",
            "base",
        ]);
        let info = worktree::create_worktree(root, run_id, "main").unwrap();
        (repo, info)
    }

    #[test]
    fn reclaim_worktrees_removes_the_directory_and_reports_it() {
        let (repo, info) = fixture_repo_with_worktree("a");
        // `keep` is how many to KEEP — `keep: 0` means "unlimited, never reclaim"
        // ([`treats_keep_zero_as_unlimited`]), so a second, newer, kept-by-budget run is
        // needed to actually exercise reclamation with `keep: 1`.
        let runs = vec![
            done_with_worktree("a", "2026-07-01T00:00:00Z", &info.path),
            done_with_worktree("z", "2026-07-02T00:00:00Z", "/nonexistent/z"),
        ];
        let reclaimed =
            reclaim_worktrees(repo.path(), &runs, 1, || "2026-08-01T00:00:00Z".to_owned());
        assert_eq!(
            reclaimed,
            vec![("a".to_owned(), "2026-08-01T00:00:00Z".to_owned())]
        );
        assert!(!Path::new(&info.path).exists());
        // The branch is kept — reclaiming a directory must not lose the work.
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(["show-ref", "--verify", "--quiet", "refs/heads/duck/a"])
                .status()
                .unwrap()
                .success()
        );
    }

    fn done_with_worktree(id: &str, finished_at: &str, worktree_path: &str) -> RunRecord {
        RunRecord {
            worktree_path: Some(worktree_path.to_owned()),
            ..done(id, finished_at)
        }
    }

    #[test]
    fn rematerialize_reclaimed_worktree_recreates_a_reclaimed_directory() {
        let (repo, info) = fixture_repo_with_worktree("b");
        let runs = vec![
            done_with_worktree("b", "2026-07-01T00:00:00Z", &info.path),
            done_with_worktree("z", "2026-07-02T00:00:00Z", "/nonexistent/z"),
        ];
        let reclaimed =
            reclaim_worktrees(repo.path(), &runs, 1, || "2026-08-01T00:00:00Z".to_owned());
        assert_eq!(
            reclaimed,
            vec![("b".to_owned(), "2026-08-01T00:00:00Z".to_owned())]
        );

        let mut run = runs.into_iter().next().unwrap();
        run.worktree_reclaimed_at = Some(reclaimed[0].1.clone());
        run.base_branch = Some("main".to_owned());

        let rematerialized = rematerialize_reclaimed_worktree(repo.path(), &run).unwrap();
        assert_eq!(rematerialized.path, info.path);
        assert!(Path::new(&rematerialized.path).exists());
    }

    #[test]
    fn rematerialize_reclaimed_worktree_is_none_when_the_directory_still_exists() {
        let (repo, info) = fixture_repo_with_worktree("c");
        let mut run = done_with_worktree("c", "2026-07-01T00:00:00Z", &info.path);
        run.worktree_reclaimed_at = Some("2026-08-01T00:00:00Z".to_owned());
        assert!(rematerialize_reclaimed_worktree(repo.path(), &run).is_none());
    }
}
