//! Git shell-out layer for worktrees, base-ref resolution, autosave commits, diff/stat helpers,
//! and ref-safety checks. It deliberately shells out to the real `git` binary rather than using
//! a Git library because command behavior is part of the compatibility surface.

//! [`run_git`] is the shared command implementation every submodule calls through.

pub mod diff_base;
pub mod refs;
pub mod worktree;

use std::path::Path;
use std::process::Command;

/// The result of one `git` invocation.
#[derive(Debug, Clone)]
pub(crate) struct GitResult {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `git <args>` in `cwd`. Never panics on a failing git invocation — a nonzero exit or
/// a git binary that can't even be spawned both come back as `ok: false`; degradation is
/// the caller's policy.
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
