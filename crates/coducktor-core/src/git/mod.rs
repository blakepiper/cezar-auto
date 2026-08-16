//! The git shell-out layer, ported from `packages/cezar/src/{git-worktree,git-diff-base,
//! git-refs}.ts` (spec §11.1, step B3): worktrees, base-ref resolution, autosave commits,
//! diff, shortstat, and the ref-safety guard. **Shells out to the real `git` binary** —
//! deliberately, per the spec ("the current behavior is subtle and the shell-outs are the
//! spec"), not `git2`/`gix`.
//!
//! `packages/cezar/src/server/git.ts` and `packages/cezar/src/server/git-changes.ts`
//! (repo-info/status/log for the Repo tab, and the Changes/Files tabs/branch/commit/push
//! plumbing) are **not** ported here — neither is named in the spec's B3 ship list, and
//! both are server-route-adjacent logic that lands at B9 (`cezar-server`, "handlers stay
//! thin, delegate to cezar-core").
//!
//! TS has three near-identical private `git()` shell-out wrappers, one per file, because
//! that tree grew organically (`git-diff-base.ts`'s own doc comment notes this — it takes
//! a caller-supplied runner rather than picking one). Rust doesn't need to repeat that:
//! [`run_git`] is the one implementation every submodule here calls through.

pub mod diff_base;
pub mod refs;
pub mod worktree;

use std::path::Path;
use std::process::Command;

/// The result of one `git` invocation. Mirrors the shape every TS `git()` wrapper returns —
/// `ok`/`stdout`/`stderr`, never a thrown error.
#[derive(Debug, Clone)]
pub(crate) struct GitResult {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `git <args>` in `cwd`. Never panics on a failing git invocation — a nonzero exit or
/// a git binary that can't even be spawned both come back as `ok: false`; degradation is
/// the caller's policy, exactly as in the TS `git()` helpers this replaces.
pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> GitResult {
    match Command::new("git").current_dir(cwd).args(args).output() {
        Ok(output) => GitResult {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(err) => GitResult {
            ok: false,
            stdout: String::new(),
            stderr: err.to_string(),
        },
    }
}
