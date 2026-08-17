//! "Which ref anchors *this task's* diff" — the single rule every task-diff surface
//! resolves through (#751).
//!
//! The normal answer is `merge-base(baseBranch, HEAD)`: it keeps a task's diff to the
//! task's own commits even after the base branch moves on, and even after the task merges
//! the base back in.
//!
//! Two things break that answer, and both are fixed here:
//!
//! **A stale local base ref.** `RunRecord.baseBranch` is a NAME (`main`), resolved to a ref
//! once, when the worktree was created. Nothing ever fast-forwards the user's local `main`
//! — coducktor's agents only ever `git fetch`, which moves `origin/main` — so on a repo the
//! user does not pull, the local ref drifts arbitrarily far behind. The merge-base then
//! collapses onto that stale tip and every upstream commit the task forked from or merged
//! in counts as the task's own work. So the base is re-resolved to the freshest ref it
//! names at every call ([`freshest_base_ref`]) — the read-time twin of
//! `worktree::resolve_base_ref`, which does the same thing at worktree-creation time and
//! cannot know what happens later.
//!
//! **A repointed HEAD.** coducktor hands the agent a worktree on the task's own branch, but
//! nothing stops the agent from checking out another branch in it. The merge-base then
//! silently redefines "this task's diff" as *the whole checked-out branch*. The honest
//! anchor for a repointed HEAD is the branch as it stood **when this run first saw it** —
//! its `<branch>@{<run start>}` reflog state. Where that baseline is itself stale (the run
//! merged the base branch in afterwards), the ordinary merge-base is the tighter answer, so
//! the two candidates compete and the one that attributes FEWER CHANGED LINES to the task
//! wins.
//!
//! It lives here rather than folded into `worktree.rs` so every diff surface shares the same
//! pure decision logic; callers provide their own Git runner.

use super::refs::is_safe_git_ref;

/// What a caller's Git runner must answer with.
pub struct GitRunResult {
    pub ok: bool,
    pub stdout: String,
}

/// A caller-supplied `git` invocation, already bound to a working directory.
pub type GitRunner<'a> = &'a dyn Fn(&[&str]) -> GitRunResult;

/// The task branch coducktor created vs. the branch HEAD actually sits on.
pub struct RepointedHead {
    pub head_branch: String,
    pub task_branch: String,
}

pub struct TaskDiffBase {
    /// The ref to diff against.
    pub base: String,
    /// Present only when HEAD left the task's branch — the reason `base` is what it is.
    pub repointed_head: Option<RepointedHead>,
}

/// Options for [`resolve_task_diff_base`]. Both fields mirror the TS function's optional
/// `opts` object.
#[derive(Default)]
pub struct TaskDiffBaseOpts<'a> {
    pub task_branch: Option<&'a str>,
    pub run_started_at: Option<&'a str>,
}

/// ISO-8601 instants only, for the `<branch>@{<date>}` revision below. The timestamp comes
/// from a stored run record, and a git revision expression is the one place a stray value
/// would be interpreted rather than compared — `@{-1}` is "the previously checked-out
/// branch", not a date. Anything that is not plainly a timestamp simply disables the
/// baseline anchor. Deliberately more permissive than `time::is_zod_datetime` (that one
/// mirrors the persisted `Z`-only datetime format; this hand-rolled check also accepts a
/// `+HH:MM`/`-HH:MM` offset).
fn is_iso_instant(s: &str) -> bool {
    fn digits(bytes: &[u8], i: &mut usize, n: usize) -> bool {
        if *i + n > bytes.len() || !bytes[*i..*i + n].iter().all(u8::is_ascii_digit) {
            return false;
        }
        *i += n;
        true
    }

    let bytes = s.as_bytes();
    let mut i = 0;
    if !digits(bytes, &mut i, 4) || bytes.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b'T') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !digits(bytes, &mut i, 2) {
        return false;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if bytes.get(i) == Some(&b'Z') {
        return i + 1 == bytes.len();
    }
    match bytes.get(i) {
        Some(b'+') | Some(b'-') => {
            i += 1;
            if !digits(bytes, &mut i, 2) || bytes.get(i) != Some(&b':') {
                return false;
            }
            i += 1;
            digits(bytes, &mut i, 2) && i == bytes.len()
        }
        _ => false,
    }
}

/// The freshest ref the configured base branch names: `origin/<base>` when the local
/// branch is behind it (or missing entirely), the local branch otherwise. Same rule as
/// `worktree::resolve_base_ref`, which picks the fork point when the worktree is created;
/// this one re-applies it every time a diff is measured.
fn freshest_base_ref(run_git: GitRunner, base: &str) -> String {
    if !is_safe_git_ref(base) || base == "HEAD" || base.starts_with("origin/") {
        return base.to_owned();
    }
    let remote = format!("origin/{base}");
    let has_remote = run_git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{remote}^{{commit}}"),
    ]);
    if !has_remote.ok {
        return base.to_owned();
    }
    // Exits 0 iff local is equal to or ahead of origin.
    let local_current = run_git(&["merge-base", "--is-ancestor", &remote, base]);
    if local_current.ok {
        base.to_owned()
    } else {
        remote
    }
}

/// Where the repointed branch stood when this run started — its own reflog, read at a
/// point in time. `None` (→ the caller keeps the conservative `HEAD` anchor) when there is
/// no run start to read at, when HEAD is detached, or when the reflog doesn't reach that
/// far back.
fn checkout_baseline(
    run_git: GitRunner,
    head_branch: &str,
    run_started_at: Option<&str>,
) -> Option<String> {
    let run_started_at = run_started_at?;
    if !is_iso_instant(run_started_at) {
        return None;
    }
    if head_branch == "HEAD" || !is_safe_git_ref(head_branch) {
        return None;
    }
    let at = run_git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{head_branch}@{{{run_started_at}}}^{{commit}}"),
    ]);
    let sha = at.stdout.trim();
    if at.ok && !sha.is_empty() {
        Some(sha.to_owned())
    } else {
        None
    }
}

/// How many changed lines an anchor attributes to the task — the comparison that picks
/// between two candidates. `None` on a failing `git diff`. Deliberately its own small scan
/// rather than a call into `worktree::parse_shortstat`, keeping this helper independent of the
/// worktree module.
fn changed_lines(run_git: GitRunner, ref_: &str) -> Option<u64> {
    let res = run_git(&["diff", "--shortstat", ref_]);
    if !res.ok {
        return None;
    }
    // git's stable shortstat wording puts the count immediately before
    // "insertion(s)(+)"/"deletion(s)(-)" (" 3 files changed, 10 insertions(+), 2 deletions(-)").
    let tokens: Vec<&str> = res.stdout.split_whitespace().collect();
    let total = tokens
        .windows(2)
        .filter(|pair| pair[1].starts_with("insertion") || pair[1].starts_with("deletion"))
        .filter_map(|pair| pair[0].parse::<u64>().ok())
        .sum();
    Some(total)
}

/// Resolve the diff anchor for a task worktree. Never fails: a failing git call degrades to
/// the base branch name.
///
/// `opts.task_branch` is optional because not every caller has one (the main working tree
/// has no task branch at all). Without it there is nothing to compare HEAD against, so the
/// merge-base anchor is used unchanged.
///
/// `opts.run_started_at` is what makes the repointed-HEAD answer a measurement instead of a
/// guess; without it the anchor stays `HEAD` (uncommitted work only) — the conservative
/// #751 answer that can never claim someone else's commits.
pub fn resolve_task_diff_base(
    run_git: GitRunner,
    base_branch: &str,
    opts: TaskDiffBaseOpts,
) -> TaskDiffBase {
    let base = freshest_base_ref(run_git, base_branch);
    let merge_base = || -> String {
        let res = run_git(&["merge-base", &base, "HEAD"]);
        let sha = res.stdout.trim();
        if res.ok && !sha.is_empty() {
            sha.to_owned()
        } else {
            base.clone()
        }
    };

    let Some(task_branch) = opts.task_branch else {
        return TaskDiffBase {
            base: merge_base(),
            repointed_head: None,
        };
    };

    let head_branch_result = run_git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let head_branch = if head_branch_result.ok {
        head_branch_result.stdout.trim().to_owned()
    } else {
        String::new()
    };
    // An unreadable HEAD is NOT evidence of a repoint — fall through to the merge-base
    // anchor rather than narrowing on a guess. A detached HEAD, on the other hand, is by
    // definition not the task's branch and takes the repointed path below.
    if head_branch.is_empty() || head_branch == task_branch {
        return TaskDiffBase {
            base: merge_base(),
            repointed_head: None,
        };
    }

    let repointed_head = Some(RepointedHead {
        head_branch: head_branch.clone(),
        task_branch: task_branch.to_owned(),
    });
    let Some(baseline) = checkout_baseline(run_git, &head_branch, opts.run_started_at) else {
        return TaskDiffBase {
            base: "HEAD".to_owned(),
            repointed_head,
        };
    };

    // Two defensible anchors, so report the tighter one.
    let anchor = merge_base();
    let Some(via_baseline) = changed_lines(run_git, &baseline) else {
        return TaskDiffBase {
            base: anchor,
            repointed_head,
        };
    };
    let via_merge_base = changed_lines(run_git, &anchor);
    match via_merge_base {
        Some(via_merge_base) if via_baseline > via_merge_base => TaskDiffBase {
            base: anchor,
            repointed_head,
        },
        _ => TaskDiffBase {
            base: baseline,
            repointed_head,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_iso_instant_accepts_z_and_offset_forms() {
        assert!(is_iso_instant("2026-07-25T10:15:00.000Z"));
        assert!(is_iso_instant("2026-07-25T10:15:00Z"));
        assert!(is_iso_instant("2026-07-25T10:15:00+02:00"));
        assert!(is_iso_instant("2026-07-25T10:15:00-05:30"));
    }

    #[test]
    fn is_iso_instant_rejects_non_timestamps() {
        assert!(!is_iso_instant("not-a-date"));
        assert!(!is_iso_instant("@{-1}"));
        assert!(!is_iso_instant("2026-07-25T10:15:00"));
        assert!(!is_iso_instant(""));
    }
}
