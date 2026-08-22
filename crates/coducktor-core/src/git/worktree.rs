//! Git worktree per task. Each run
//! gets its own branch `duck/<id8>` checked out into `.ai/coducktor/worktrees/<runId>` so
//! agents never touch the user's working tree. Everything degrades: helpers never fail
//! except [`create_worktree`], whose failure the caller turns into a note.
//!
//! The writer emits `duck/`; readers retain compatibility with the former task-branch prefix.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use coducktor_contract::runs::DiffStat;

use super::diff_base::{self, TaskDiffBaseOpts};
use super::refs::is_safe_git_ref;
use super::run_git;

/// Repo-relative home of all task worktrees (gitignored via `.ai/coducktor/.gitignore`).
pub const WORKTREES_DIR: &str = ".ai/coducktor/worktrees";

/// Cap on `worktree_diff` output.
pub const DIFF_CAP: usize = 400_000;

/// Largest file the autosave conflict-marker scan will read. Autosave runs as often as
/// every 90s, and reading a large tracked file into memory each time is a real cost for a
/// check that only ever looks at a few marker lines. Oversized files are skipped, which
/// fails *open* — a false negative here costs noise, a false positive costs the recovery
/// point.
const MARKER_SCAN_MAX_BYTES: u64 = 2_000_000;

#[derive(Debug, Clone)]
struct RegisteredWorktree {
    path: String,
    branch: Option<String>,
}

pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    /// Branch name the worktree was forked from (a commit sha when HEAD was detached).
    pub base_branch: String,
}

fn worktree_info(path: String, branch: String, base_branch: String) -> WorktreeInfo {
    WorktreeInfo {
        path,
        branch,
        base_branch,
    }
}

pub fn branch_for(run_id: &str) -> String {
    let id8: String = run_id.chars().take(8).collect();
    format!("duck/{id8}")
}

/// Resolve the configured base branch to something `git worktree add` can fork from: the
/// local branch, its remote-tracking ref, or `None` — the caller falls back to the current
/// branch with a note. Never fails.
///
/// When BOTH the local branch and `origin/<base>` exist, prefer whichever is up to date. A
/// stale LOCAL base (behind origin) is the classic inflated-diff trap: the merge-base every
/// diff is measured from collapses onto the stale tip, so all of the history merged into
/// origin since then counts as the task's own changes. `origin/<base>` is the source of
/// truth for a review base, so only keep the local ref when it is equal to or ahead of
/// origin; otherwise use origin.
///
/// This answers the question once, when the worktree is forked. The local ref goes stale
/// AFTERWARDS too, so every diff re-applies the same rule at read time through
/// `diff_base::resolve_task_diff_base`'s `freshest_base_ref` — keep the two in agreement.
pub fn resolve_base_ref(repo_root: &Path, base: &str) -> Option<String> {
    if !is_safe_git_ref(base) {
        return None;
    }
    let verify = |ref_: &str| {
        run_git(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{ref_}^{{commit}}"),
            ],
        )
        .ok
    };
    let has_local = verify(base);
    let remote = format!("origin/{base}");
    let has_remote = verify(&remote);
    if has_local && has_remote {
        // `--is-ancestor origin/<base> <base>` succeeds iff local is equal-or-ahead.
        let local_current = run_git(repo_root, &["merge-base", "--is-ancestor", &remote, base]);
        return Some(if local_current.ok {
            base.to_owned()
        } else {
            remote
        });
    }
    if has_local {
        return Some(base.to_owned());
    }
    if has_remote {
        return Some(remote);
    }
    None
}

pub fn worktree_path_for(repo_root: &Path, run_id: &str) -> PathBuf {
    repo_root.join(WORKTREES_DIR).join(run_id)
}

/// Parse `git worktree list --porcelain` without assuming paths contain no spaces —
/// records are terminated by a blank line, not whitespace.
fn registered_worktrees(repo_root: &Path) -> Vec<RegisteredWorktree> {
    let res = run_git(repo_root, &["worktree", "list", "--porcelain"]);
    if !res.ok {
        return Vec::new();
    }
    let mut worktrees = Vec::new();
    let mut current: Option<RegisteredWorktree> = None;
    for line in res.stdout.split('\n') {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(prev) = current.take() {
                worktrees.push(prev);
            }
            current = Some(RegisteredWorktree {
                path: path.to_owned(),
                branch: None,
            });
        } else if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(cur) = current.as_mut() {
                cur.branch = Some(branch.to_owned());
            }
        } else if line.is_empty()
            && let Some(prev) = current.take()
        {
            worktrees.push(prev);
        }
    }
    if let Some(prev) = current {
        worktrees.push(prev);
    }
    worktrees
}

/// Git canonicalizes symlinked path prefixes (macOS `/var` → `/private/var`); fall back to
/// an absolutized (not necessarily existing) path when the target doesn't exist yet.
fn canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Establish the task worktree idempotently. Besides the fresh `git worktree add -b` path,
/// recover the two normal restart/deletion cases: reuse an already-registered task
/// worktree, or reattach a surviving task branch after its directory/registration was
/// removed. Existing non-empty unregistered paths are never deleted because they may hold
/// recoverable uncommitted work. The one function here that fails loudly.
pub fn create_worktree(
    repo_root: &Path,
    run_id: &str,
    base_branch: &str,
) -> Result<WorktreeInfo, String> {
    let mut base = base_branch.to_owned();
    if base.is_empty() || base == "HEAD" {
        // Detached HEAD — pin the base to the current commit so the record and later
        // diffs stay meaningful.
        let head = run_git(repo_root, &["rev-parse", "HEAD"]);
        if !head.ok {
            return Err(format!("git rev-parse HEAD failed: {}", head.stderr.trim()));
        }
        base = head.stdout.trim().to_owned();
    }
    // Dash-guard (#431): `base` is spliced in as a positional git operand; an option-like
    // value would be argument injection.
    if !is_safe_git_ref(&base) {
        return Err(format!("refusing option-like base ref: {base}"));
    }
    let branch = branch_for(run_id);
    let absolute_path = canonical_path(repo_root).join(WORKTREES_DIR).join(run_id);
    let branch_ref = format!("refs/heads/{branch}");

    // A missing directory can leave stale administrative metadata behind. Prune first so
    // the checks below describe the filesystem as it exists now.
    run_git(repo_root, &["worktree", "prune"]);
    let canonical_target = canonical_path(&absolute_path);
    let mut registered = registered_worktrees(repo_root);
    let find_at_path = |registered: &[RegisteredWorktree]| {
        registered
            .iter()
            .find(|item| canonical_path(Path::new(&item.path)) == canonical_target)
            .cloned()
    };
    if let Some(at_path) = find_at_path(&registered) {
        if at_path.branch.as_deref() != Some(branch_ref.as_str()) {
            return Err(format!(
                "managed worktree path is registered to {}, expected {branch_ref}",
                at_path.branch.as_deref().unwrap_or("a detached HEAD")
            ));
        }
        return Ok(worktree_info(at_path.path, branch, base));
    }

    // If the directory survived but its administrative entry did not, let Git repair it.
    // This is non-destructive; an unrepairable non-empty directory is preserved and
    // reported instead of being recursively removed.
    if absolute_path.exists() {
        run_git(
            repo_root,
            &["worktree", "repair", &absolute_path.to_string_lossy()],
        );
        registered = registered_worktrees(repo_root);
        if let Some(at_path) = find_at_path(&registered) {
            if at_path.branch.as_deref() != Some(branch_ref.as_str()) {
                return Err(format!(
                    "managed worktree path is registered to {}, expected {branch_ref}",
                    at_path.branch.as_deref().unwrap_or("a detached HEAD")
                ));
            }
            return Ok(worktree_info(at_path.path, branch, base));
        }
        let non_empty = fs::read_dir(&absolute_path)
            .map(|rd| rd.count() > 0)
            .unwrap_or(true);
        if non_empty {
            return Err(format!(
                "managed worktree path already exists and could not be repaired: {}",
                absolute_path.display()
            ));
        }
    }

    let branch_exists = run_git(repo_root, &["show-ref", "--verify", "--quiet", &branch_ref]);
    if branch_exists.ok {
        // The task branch may still be checked out after its directory was moved. Reuse
        // only a path whose basename is the full run id; a same-prefix collision must fail
        // rather than point two tasks at one worktree.
        let by_branch = registered
            .iter()
            .find(|item| item.branch.as_deref() == Some(branch_ref.as_str()));
        if let Some(by_branch) = by_branch {
            let basename = Path::new(&by_branch.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if basename != run_id {
                return Err(format!(
                    "task branch {branch} is already checked out at {}",
                    by_branch.path
                ));
            }
            return Ok(worktree_info(by_branch.path.clone(), branch, base));
        }
        let absolute_path_str = absolute_path.to_string_lossy().into_owned();
        let attach = run_git(repo_root, &["worktree", "add", &absolute_path_str, &branch]);
        if !attach.ok {
            let msg = if !attach.stderr.trim().is_empty() {
                attach.stderr.trim()
            } else {
                attach.stdout.trim()
            };
            return Err(format!("git worktree reattach failed: {msg}"));
        }
        return Ok(worktree_info(absolute_path_str, branch, base));
    }

    let absolute_path_str = absolute_path.to_string_lossy().into_owned();
    let create = run_git(
        repo_root,
        &["worktree", "add", "-b", &branch, &absolute_path_str, &base],
    );
    if !create.ok {
        let msg = if !create.stderr.trim().is_empty() {
            create.stderr.trim()
        } else {
            create.stdout.trim()
        };
        return Err(format!("git worktree add failed: {msg}"));
    }
    Ok(worktree_info(absolute_path_str, branch, base))
}

/// Best-effort on-disk size of a worktree directory in bytes, via POSIX `du -sk` (kibibytes
/// → bytes). `None` when `du` is unavailable — including all of Windows — or on any error.
/// Never fails and never blocks long: worktree retention is count-based, so a missing size
/// only blanks the panel's size column, it does not affect reclamation.
pub fn worktree_size_bytes(path: &Path) -> Option<u64> {
    let output = Command::new("du").arg("-sk").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let kib: u64 = stdout.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// Remove a task worktree and its branch. Best effort — never fails.
pub fn remove_worktree(repo_root: &Path, worktree_path: &Path, branch: Option<&str>) {
    run_git(
        repo_root,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
    );
    let _ = fs::remove_dir_all(worktree_path);
    run_git(repo_root, &["worktree", "prune"]);
    if let Some(branch) = branch {
        run_git(repo_root, &["branch", "-D", branch]);
    }
}

/// Why an autosave commit happened. Production recovery uses the turn-end, run-finalize, and
/// pre-PR boundaries; `Periodic` remains available to explicit callers and tests. The message
/// carries the reason so each recovery boundary is distinguishable in `git log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveReason {
    Periodic,
    TurnEnd,
    RunFinalize,
    PrePr,
}

impl AutosaveReason {
    fn label(self) -> &'static str {
        match self {
            AutosaveReason::Periodic => "periodic",
            AutosaveReason::TurnEnd => "turn end",
            AutosaveReason::RunFinalize => "run finalize",
            AutosaveReason::PrePr => "pre-PR",
        }
    }
}

/// What an autosave attempt did. `Refused` and `Failed` are distinct from `NothingToDo`
/// because one call site must act on them: the pre-PR flush is the *last* one, so anything
/// other than `Committed`/`NothingToDo` there means the branch leaves the box without the
/// run's final state and the user has to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveResult {
    Committed,
    NothingToDo,
    Refused,
    Failed,
}

/// Leftover merge-conflict markers, anchored at line start as git writes them. All three
/// must appear *in order*: a bare `=======` line is ordinary Markdown (a setext heading
/// underline), and any of the three alone occurs legitimately in checked-in patch fixtures
/// and docs that demonstrate conflict markers. Requiring the full ordered triple is what
/// separates a real conflict from a file that merely talks about one.
fn is_conflict_marker_line(line: &str, marker: &str) -> bool {
    line.strip_prefix(marker)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()))
}

fn has_conflict_markers(text: &str) -> bool {
    #[derive(PartialEq, Eq)]
    enum Want {
        Start,
        Mid,
        End,
    }
    let mut want = Want::Start;
    for line in text.split('\n') {
        want = match want {
            Want::Start if is_conflict_marker_line(line, "<<<<<<<") => Want::Mid,
            Want::Mid if is_conflict_marker_line(line, "=======") => Want::End,
            Want::End if is_conflict_marker_line(line, ">>>>>>>") => return true,
            other => other,
        };
    }
    false
}

/// Is the worktree mid-merge or still carrying conflict markers? Returns a human-readable
/// reason, or `None` when it is safe to autosave.
///
/// Two independent checks, because they catch different failures: porcelain `U` codes catch
/// an *unresolved* merge, while the marker scan catches the worse case — someone ran
/// `git add` on a file they had not finished resolving, which clears the `U` code but
/// leaves `<<<<<<<` in the text.
fn unresolved_conflicts(dir: &Path, porcelain: &str) -> Option<String> {
    let entries: Vec<&str> = porcelain.split('\n').filter(|l| !l.is_empty()).collect();
    let unmerged: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|line| {
            let xy = line.get(0..2).unwrap_or("");
            xy.contains('U') || xy == "AA" || xy == "DD"
        })
        .collect();
    if let Some(first) = unmerged.first() {
        return Some(format!(
            "{} unmerged path(s), e.g. {}",
            unmerged.len(),
            first.get(3..).unwrap_or("")
        ));
    }
    // Deleted paths have no content to scan. Untracked (`??`) ones are skipped even though
    // `git add -A` will commit them: a conflict always lands in a tracked file, whereas an
    // untracked fixture or doc that legitimately contains marker-shaped lines would
    // otherwise wedge every autosave. False negatives here cost noise; false positives
    // would cost the recovery point.
    let candidates: Vec<String> = entries
        .iter()
        .filter(|line| {
            !line.starts_with("D ") && !line.starts_with(" D") && !line.starts_with("??")
        })
        .map(|line| {
            let raw = line.get(3..).unwrap_or("").trim();
            // A rename reads `old -> new`; the new path is what gets committed.
            let path = raw.find(" -> ").map_or(raw, |idx| &raw[idx + 4..]);
            unquote_path(path)
        })
        .collect();
    for path in candidates {
        // Read the working tree, not the index: this runs before `git add -A`, so the
        // on-disk copy is what would be committed.
        let full = dir.join(&path);
        let Ok(meta) = fs::metadata(&full) else {
            continue;
        };
        if meta.len() > MARKER_SCAN_MAX_BYTES {
            continue;
        }
        let Ok(text) = fs::read_to_string(&full) else {
            continue;
        };
        if has_conflict_markers(&text) {
            return Some(format!("leftover conflict markers in {path}"));
        }
    }
    None
}

/// Undo `git status --porcelain` path quoting. Git quotes a path containing a space or a
/// non-ASCII byte, escaping the latter as octal — `café.txt` reads back as
/// `"caf\303\251.txt"`. Stripping the quotes alone leaves those escapes literal, so the file
/// would not open and a real conflict in it would go unnoticed. Decoding the octal bytes as
/// UTF-8 restores the actual name.
fn unquote_path(path: &str) -> String {
    if !(path.starts_with('"') && path.ends_with('"')) || path.len() < 2 {
        return path.to_owned();
    }
    let inner = &path.as_bytes()[1..path.len() - 1];
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0usize;
    while i < inner.len() {
        if inner[i] != b'\\' {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        let Some(&next) = inner.get(i + 1) else { break };
        if let Some(octal) = inner.get(i + 1..i + 4)
            && octal.len() == 3
            && octal.iter().all(|b| (b'0'..=b'7').contains(b))
        {
            out.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'));
            i += 4;
            continue;
        }
        // C-style single-character escapes git also emits.
        match next {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            other => out.push(other),
        }
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Does this repo/worktree resolve a git author identity (name + email)? Ambient config
/// wins so autosave commits carry the user's own identity.
fn git_has_identity(dir: &Path) -> bool {
    let name = run_git(dir, &["config", "user.name"]);
    let email = run_git(dir, &["config", "user.email"]);
    name.ok && !name.stdout.trim().is_empty() && email.ok && !email.stdout.trim().is_empty()
}

/// Stage and commit everything in the worktree as a "coducktor autosave" commit (janitor
/// pattern) — the agent's progress is always recoverable from the `duck/<id8>` branch
/// history. Quietly a no-op when nothing changed.
///
/// Refuses to commit a worktree that is mid-merge or still carries conflict markers: a
/// blind `git add -A` would capture a half-resolved merge (#471).
pub fn autosave_commit(dir: &Path, reason: AutosaveReason) -> AutosaveResult {
    let status = run_git(dir, &["status", "--porcelain"]);
    if !status.ok || status.stdout.trim().is_empty() {
        return AutosaveResult::NothingToDo;
    }
    if let Some(unresolved) = unresolved_conflicts(dir, &status.stdout) {
        // Losing one recovery point beats poisoning the branch with a commit that does not
        // build. For the periodic and turn-end flushes the next one picks the work up once
        // the merge resolves; the pre-PR flush has no next one.
        eprintln!(
            "[coducktor] skipping {} autosave in {}: {unresolved}",
            reason.label(),
            dir.display()
        );
        return AutosaveResult::Refused;
    }
    run_git(dir, &["add", "-A"]);
    // Commit as the CURRENT git user so the branch's commits (and any PR opened from it)
    // are attributed to the real author. Fall back to a fixed identity ONLY when the
    // machine has none configured — otherwise `git commit` would fail and the autosave (the
    // run's recovery point) would be lost.
    let message = format!("coducktor autosave ({})", reason.label());
    let commit = if git_has_identity(dir) {
        run_git(dir, &["commit", "--no-verify", "-m", &message])
    } else {
        run_git(
            dir,
            &[
                "-c",
                "user.name=coducktor",
                "-c",
                "user.email=coducktor@local",
                "commit",
                "--no-verify",
                "-m",
                &message,
            ],
        )
    };
    if commit.ok {
        AutosaveResult::Committed
    } else {
        AutosaveResult::Failed
    }
}

fn resolved_diff_base(worktree_path: &Path, base_branch: &str) -> String {
    run_git(worktree_path, &["add", "-N", "."]); // intent-to-add: untracked files show up
    let merge_base = run_git(worktree_path, &["merge-base", base_branch, "HEAD"]);
    if merge_base.ok && !merge_base.stdout.trim().is_empty() {
        merge_base.stdout.trim().to_owned()
    } else {
        base_branch.to_owned()
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// "What did this task change": diff of the worktree (committed + uncommitted + untracked)
/// against the merge-base with its base branch — so the diff stays *this task's* changes
/// even after the base moves on.
///
/// Deliberately does NOT take the repointed-HEAD guard that [`worktree_shortstat`] resolves
/// through `diff_base::resolve_task_diff_base` — this is the whole-branch anchor on
/// purpose. Its text output is a compatibility-sensitive durable run diff, so narrowing it would
/// silently change what existing consumers read.
pub fn worktree_diff(worktree_path: &Path, base_branch: &str, cap: usize) -> String {
    if !is_safe_git_ref(base_branch) {
        return "(diff failed: refusing option-like base ref)".to_owned();
    }
    let base = resolved_diff_base(worktree_path, base_branch);
    let res = run_git(worktree_path, &["diff", &base]);
    if !res.ok {
        let err = if !res.stderr.trim().is_empty() {
            res.stderr.trim()
        } else {
            "unknown git error"
        };
        return format!("(diff failed: {err})");
    }
    if res.stdout.len() > cap {
        format!("{}\n… (diff truncated)", truncate_utf8(&res.stdout, cap))
    } else {
        res.stdout
    }
}

/// `git diff --stat` version of [`worktree_diff`]. Same merge-base anchoring, and stays
/// whole-branch for the same reason: variants are sibling worktrees on their own `duck/*`
/// branches, and the column exists to compare
/// their *committed* work against one another. Returns `""` on any failure.
pub fn worktree_diff_stat(worktree_path: &Path, base_branch: &str) -> String {
    if !is_safe_git_ref(base_branch) {
        return String::new();
    }
    let base = resolved_diff_base(worktree_path, base_branch);
    let res = run_git(worktree_path, &["diff", "--stat", &base]);
    if res.ok {
        res.stdout.trim().to_owned()
    } else {
        String::new()
    }
}

fn leading_number(s: &str) -> Option<u64> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Parse `git diff --shortstat` output — " 3 files changed, 10 insertions(+), 2
/// deletions(-)". Every part is optional: insertions-only and deletions-only diffs omit the
/// other counter, and an empty diff prints nothing at all (→ all zeros). The wording is
/// stable porcelain English — git does not localize `--shortstat` — so matching the words is
/// safe.
pub fn parse_shortstat(s: &str) -> DiffStat {
    let mut files = 0u64;
    let mut adds = 0u64;
    let mut dels = 0u64;
    for part in s.split(',') {
        let part = part.trim();
        let Some(n) = leading_number(part) else {
            continue;
        };
        if part.contains("file") {
            files = n;
        } else if part.contains("insertion") {
            adds = n;
        } else if part.contains("deletion") {
            dels = n;
        }
    }
    DiffStat {
        adds: adds as f64,
        dels: dels as f64,
        files: files as f64,
        repointed: None,
    }
}

/// `git diff --shortstat` of the worktree vs its base (#389) — the numbers behind
/// `RunRecord.diffStat`. Same intent-to-add as [`worktree_diff`], but the anchor comes from
/// `diff_base::resolve_task_diff_base`: pass the run's own `task_branch`/`run_started_at`,
/// and a worktree whose HEAD was repointed onto another branch reports what this run did to
/// that branch instead of claiming the branch's whole diff as this task's (#751).
///
/// `repointed: Some(true)` rides along on the returned stat exactly when that narrowing
/// happened — never `Some(false)` on a normal run, so the persisted `runs.json` shape keeps
/// round-tripping byte-identically. `None` on git failure; an empty diff is a valid
/// all-zero stat.
pub fn worktree_shortstat(
    worktree_path: &Path,
    base_branch: &str,
    task_branch: Option<&str>,
    run_started_at: Option<&str>,
) -> Option<DiffStat> {
    if !is_safe_git_ref(base_branch) {
        return None;
    }
    run_git(worktree_path, &["add", "-N", "."]); // intent-to-add: untracked files show up
    let runner = |args: &[&str]| -> diff_base::GitRunResult {
        let res = run_git(worktree_path, args);
        diff_base::GitRunResult {
            ok: res.ok,
            stdout: res.stdout,
        }
    };
    let resolved = diff_base::resolve_task_diff_base(
        &runner,
        base_branch,
        TaskDiffBaseOpts {
            task_branch,
            run_started_at,
        },
    );
    let res = run_git(worktree_path, &["diff", "--shortstat", &resolved.base]);
    if !res.ok {
        return None;
    }
    let mut stat = parse_shortstat(&res.stdout);
    if resolved.repointed_head.is_some() {
        stat.repointed = Some(true);
    }
    Some(stat)
}

/// Startup reconcile: `git worktree prune` + remove every directory under
/// `.ai/coducktor/worktrees/` whose run id is no longer in the store (and its branch).
/// Returns the removed run ids for the boot log. Never fails.
pub fn prune_orphans(repo_root: &Path, valid_ids: &HashSet<String>) -> Vec<String> {
    run_git(repo_root, &["worktree", "prune"]);
    let entries = match fs::read_dir(repo_root.join(WORKTREES_DIR)) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(), // no worktrees dir yet
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if valid_ids.contains(&name) {
            continue;
        }
        remove_worktree(
            repo_root,
            &worktree_path_for(repo_root, &name),
            Some(&branch_for(&name)),
        );
        removed.push(name);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Create a temporary repository with a base commit and explicit test identity (so autosave's
    /// identity fallback
    /// path is never exercised by accident in these tests).
    fn fixture_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ok = |args: &[&str]| assert!(run_git(root, args).ok, "git {args:?} failed");
        ok(&["init", "-q", "-b", "main"]);
        ok(&["config", "user.name", "test"]);
        ok(&["config", "user.email", "test@local"]);
        fs::write(root.join("base.txt"), "base\n").unwrap();
        ok(&["add", "-A"]);
        ok(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-q",
            "-m",
            "base",
        ]);
        dir
    }

    fn commit_all(root: &Path, message: &str) {
        assert!(run_git(root, &["add", "-A"]).ok);
        assert!(
            run_git(
                root,
                &[
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@local",
                    "commit",
                    "-q",
                    "-m",
                    message
                ]
            )
            .ok
        );
    }

    #[test]
    fn branch_for_takes_the_first_eight_chars() {
        assert_eq!(branch_for("ab12cd34-ef56"), "duck/ab12cd34");
        assert_eq!(branch_for("short"), "duck/short");
    }

    #[test]
    fn parse_shortstat_handles_partial_and_empty_output() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"),
            DiffStat {
                adds: 10.0,
                dels: 2.0,
                files: 3.0,
                repointed: None
            }
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+)"),
            DiffStat {
                adds: 1.0,
                dels: 0.0,
                files: 1.0,
                repointed: None
            }
        );
        assert_eq!(
            parse_shortstat(""),
            DiffStat {
                adds: 0.0,
                dels: 0.0,
                files: 0.0,
                repointed: None
            }
        );
    }

    #[test]
    fn create_worktree_is_idempotent_on_a_second_call() {
        let repo = fixture_repo();
        let first = create_worktree(repo.path(), "run-one", "main").unwrap();
        let second = create_worktree(repo.path(), "run-one", "main").unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(first.branch, "duck/run-one");
        assert_eq!(second.branch, "duck/run-one");
        remove_worktree(repo.path(), Path::new(&first.path), Some(&first.branch));
    }

    #[test]
    fn create_worktree_reattaches_after_directory_loss() {
        let repo = fixture_repo();
        let info = create_worktree(repo.path(), "run-two", "main").unwrap();
        let path = PathBuf::from(&info.path);
        // Simulate the directory vanishing without git being told (a crash, a manual rm).
        fs::remove_dir_all(&path).unwrap();
        let reattached = create_worktree(repo.path(), "run-two", "main").unwrap();
        assert_eq!(reattached.branch, "duck/run-two");
        assert!(Path::new(&reattached.path).exists());
        remove_worktree(
            repo.path(),
            Path::new(&reattached.path),
            Some(&reattached.branch),
        );
    }

    #[test]
    fn resolve_base_ref_prefers_local_when_local_is_ahead_of_origin() {
        let repo = fixture_repo();
        // No `origin` remote at all in this fixture: local wins by default.
        assert_eq!(
            resolve_base_ref(repo.path(), "main").as_deref(),
            Some("main")
        );
    }

    #[test]
    fn resolve_base_ref_prefers_origin_when_local_is_stale() {
        let repo = fixture_repo();
        let remote = fixture_repo();
        // Wire `repo`'s origin at the remote's current tip, then advance the remote so
        // `repo`'s local `main` is stale relative to `origin/main`.
        assert!(
            run_git(
                repo.path(),
                &["remote", "add", "origin", &remote.path().to_string_lossy()]
            )
            .ok
        );
        assert!(run_git(repo.path(), &["fetch", "-q", "origin"]).ok);
        fs::write(remote.path().join("more.txt"), "more\n").unwrap();
        commit_all(remote.path(), "advance");
        assert!(run_git(repo.path(), &["fetch", "-q", "origin"]).ok);

        assert_eq!(
            resolve_base_ref(repo.path(), "main").as_deref(),
            Some("origin/main")
        );
    }

    #[test]
    fn resolve_base_ref_rejects_option_like_refs() {
        let repo = fixture_repo();
        assert_eq!(resolve_base_ref(repo.path(), "--upload-pack=evil"), None);
    }

    #[test]
    fn autosave_commit_is_a_noop_when_nothing_changed() {
        let repo = fixture_repo();
        assert_eq!(
            autosave_commit(repo.path(), AutosaveReason::Periodic),
            AutosaveResult::NothingToDo
        );
    }

    #[test]
    fn autosave_commit_commits_dirty_work() {
        let repo = fixture_repo();
        fs::write(repo.path().join("new.txt"), "hello\n").unwrap();
        assert_eq!(
            autosave_commit(repo.path(), AutosaveReason::TurnEnd),
            AutosaveResult::Committed
        );
        let log = run_git(repo.path(), &["log", "-1", "--pretty=%s"]);
        assert_eq!(log.stdout.trim(), "coducktor autosave (turn end)");
    }

    #[test]
    fn autosave_commit_refuses_a_mid_merge_worktree() {
        let repo = fixture_repo();
        // Two branches that touch the same line so a merge conflicts.
        assert!(run_git(repo.path(), &["checkout", "-q", "-b", "feature"]).ok);
        fs::write(repo.path().join("base.txt"), "feature\n").unwrap();
        commit_all(repo.path(), "feature change");
        assert!(run_git(repo.path(), &["checkout", "-q", "main"]).ok);
        fs::write(repo.path().join("base.txt"), "main\n").unwrap();
        commit_all(repo.path(), "main change");
        let merge = run_git(repo.path(), &["merge", "feature"]);
        assert!(!merge.ok, "expected the merge to conflict");

        assert_eq!(
            autosave_commit(repo.path(), AutosaveReason::PrePr),
            AutosaveResult::Refused
        );
    }

    #[test]
    fn worktree_shortstat_reports_zero_on_a_clean_worktree() {
        let repo = fixture_repo();
        let info = create_worktree(repo.path(), "run-three", "main").unwrap();
        let stat =
            worktree_shortstat(Path::new(&info.path), "main", Some(&info.branch), None).unwrap();
        assert_eq!(
            stat,
            DiffStat {
                adds: 0.0,
                dels: 0.0,
                files: 0.0,
                repointed: None
            }
        );
        remove_worktree(repo.path(), Path::new(&info.path), Some(&info.branch));
    }

    #[test]
    fn worktree_shortstat_counts_committed_work_in_the_worktree() {
        let repo = fixture_repo();
        let info = create_worktree(repo.path(), "run-four", "main").unwrap();
        let wt = Path::new(&info.path);
        fs::write(wt.join("extra.txt"), "line one\nline two\n").unwrap();
        commit_all(wt, "task work");
        let stat = worktree_shortstat(wt, "main", Some(&info.branch), None).unwrap();
        assert_eq!(stat.adds, 2.0);
        assert_eq!(stat.files, 1.0);
        remove_worktree(repo.path(), wt, Some(&info.branch));
    }

    #[test]
    fn worktree_size_bytes_reports_a_positive_size_for_a_real_directory() {
        let repo = fixture_repo();
        assert!(worktree_size_bytes(repo.path()).unwrap_or(0) > 0);
    }

    #[test]
    fn prune_orphans_removes_directories_not_in_the_valid_set() {
        let repo = fixture_repo();
        let info = create_worktree(repo.path(), "keep-me", "main").unwrap();
        create_worktree(repo.path(), "drop-me", "main").unwrap();
        let mut valid = HashSet::new();
        valid.insert("keep-me".to_owned());
        let removed = prune_orphans(repo.path(), &valid);
        assert_eq!(removed, vec!["drop-me".to_owned()]);
        assert!(Path::new(&info.path).exists());
        assert!(!worktree_path_for(repo.path(), "drop-me").exists());
    }
}
