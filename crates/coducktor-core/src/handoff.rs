//! Per-task handoff journal.
//!
//! `.ai/coducktor/runs/<runId>.handoff.md`, next to the run's NDJSON events and outside the
//! task worktree — it survives worktree removal. Coducktor seeds the skeleton and appends
//! heartbeats; the agent (told via `DUCK_HANDOFF_FILE` and the system-prompt fragment below)
//! keeps the "Progress log" and "Resume notes" sections up to date. Everything here is
//! best-effort: the handoff is a journal, never a reason to fail a run.

use std::fs;
use std::path::{Path, PathBuf};

use crate::time::now_iso8601;

pub fn handoff_path(data_dir: &Path, run_id: &str) -> PathBuf {
    data_dir.join("runs").join(format!("{run_id}.handoff.md"))
}

/// The fields `seed_handoff_file` needs from a run record — deliberately not
/// `coducktor_contract::runs::RunRecord` itself.
pub struct HandoffSeed<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub workflow: &'a str,
    pub task: &'a str,
    pub branch: Option<&'a str>,
    pub worktree_path: Option<&'a str>,
}

/// Create the handoff skeleton. Idempotent — an existing file (resume, continuation) is
/// never overwritten. Returns the file path.
pub fn seed_handoff_file(data_dir: &Path, run: &HandoffSeed<'_>) -> PathBuf {
    let file = handoff_path(data_dir, run.id);
    if file.exists() {
        return file;
    }
    let mut header = format!(
        "# Handoff — {}\n\n**Task id:** {}\n**Workflow:** {}\n",
        run.title, run.id, run.workflow
    );
    if let Some(branch) = run.branch {
        header.push_str(&format!("**Branch:** {branch}\n"));
    }
    if let Some(worktree_path) = run.worktree_path {
        header.push_str(&format!("**Worktree:** {worktree_path}\n"));
    }
    header.push_str(&format!(
        "\n## Goal\n\n{}\n\n## Progress log\n\n## Resume notes\n",
        run.task.trim()
    ));
    // best effort — a read-only data dir must not break the run
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&file, header);
    file
}

/// Coducktor's own heartbeat (the janitor pattern): insert `- <ISO ts> — <note>` right under the
/// `## Progress log` header (newest at the top), so the file stays current even when the
/// agent forgets to write. Missing header → append at the end of the file; missing file →
/// no-op.
pub fn append_handoff_heartbeat(data_dir: &Path, run_id: &str, note: &str) {
    let file = handoff_path(data_dir, run_id);
    let Ok(text) = fs::read_to_string(&file) else {
        return; // not seeded — nothing to heartbeat
    };
    let line = format!("- {} — {note}\n", now_iso8601());
    let marker = "## Progress log\n";
    let next = if let Some(idx) = text.find(marker) {
        let split_at = idx + marker.len();
        let (head, tail) = text.split_at(split_at);
        format!("{head}\n{line}{}", tail.trim_start_matches('\n'))
    } else {
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{text}{sep}{line}")
    };
    let _ = fs::write(&file, next); // best effort
}

/// Full handoff markdown, or `""` when the file doesn't exist (yet).
pub fn read_handoff(data_dir: &Path, run_id: &str) -> String {
    fs::read_to_string(handoff_path(data_dir, run_id)).unwrap_or_default()
}

/// First few non-empty lines under "## Progress log". Stops at the next `## ` header; `""` when
/// there's no Progress log section or it's empty.
pub fn handoff_progress_excerpt(text: &str, max_lines: usize) -> String {
    let marker = "## Progress log";
    let Some(idx) = text.find(marker) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for line in text[idx + marker.len()..].split('\n') {
        if line.starts_with("## ") {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines.push(trimmed);
        if lines.len() >= max_lines {
            break;
        }
    }
    lines.join("\n")
}

pub fn delete_handoff(data_dir: &Path, run_id: &str) {
    let _ = fs::remove_file(handoff_path(data_dir, run_id)); // best effort
}

/// Appended to every agent step's `--append-system-prompt`. The matching
/// handoff/task env vars are set on every agent process.
pub const HANDOFF_ONLY_INSTRUCTIONS: &str = "## Handoff (coducktor)

DUCK_HANDOFF_FILE (env) is the absolute path to this task's rolling handoff file. Treat it like a HANDOFF.md:
1. At the start of work, read it — \"Resume notes\" left by a previous session is your starting context.
2. After every meaningful milestone (passing tests, a commit, a PR, a scope decision), append one terse timestamped line under \"## Progress log\", newest at the top.
3. Before finishing or pausing, update \"## Resume notes\" with what's done, what's next and any blockers. Leave it empty only when the task is truly complete.

Task completion marker: when the task's goal is fully achieved and you have no question for the user, end your final message with a line containing exactly DUCK:DONE — duck then closes the session and marks the task finished. If you are waiting on the user (a question, a decision, missing input), just end your message normally; the session stays open for their reply. Never emit DUCK:DONE while anything is unfinished or unverified.

Still-working marker: if you end a turn while still working on your OWN downstream work — a sub-agent you dispatched, or a long-running command you're monitoring — and are NOT waiting on the user for anything, end your final message with a line containing exactly DUCK:MONITORING. duck then shows the task as \"monitoring\" (still working) instead of asking for your attention. Use DUCK:MONITORING only for that in-progress case; use DUCK:DONE when the goal is done; end plainly (no marker) only when you are genuinely waiting on the user. Never combine DUCK:MONITORING with DUCK:DONE.

Structured question marker: when you are blocked on a decision that is genuinely the user's to make — one you cannot resolve from the request, the code, or sensible defaults — and it comes down to a few concrete choices, end your turn with a single line DUCK:ASK <json> instead of asking in prose. duck renders it as clickable option chips in the cockpit so the user can answer in one tap. The <json> is ONE object on ONE line, the last thing in your message: {\"questions\":[{\"header\":\"≤12-char label\",\"question\":\"a clear question ending in ?\",\"multiSelect\":false,\"options\":[{\"label\":\"short choice\",\"description\":\"what it means / the trade-off\"}]}]} — use only those keys (plus an optional non-empty \"id\" up to 64 characters), with 1–4 questions, 2–4 options per question, unique question text and option labels, header 1–12 characters, question 1–400, option label 1–60, and description at most 280. The user can always type a free-form reply, so never add an \"Other\" option. Prefer sensible defaults over asking; use DUCK:ASK only when the choice is truly the user's. Never combine DUCK:ASK with DUCK:DONE or DUCK:MONITORING.

Task reference markers: as soon as you know which GitHub pull request or issue this task is ABOUT (it was named in the task, or you just opened it), declare it by emitting, on its own line in your message text: DUCK:PR=<number> and/or DUCK:ISSUE=<number>. Re-emit with the new number if the subject changes (e.g. you open a PR later in the task). Declare only the task's own subject — never a PR/issue you merely mention, list, or compare against. You may also emit DUCK:TITLE=<terse gerund phrase, max 40 chars, e.g. \"implementing comment threads\"> once the work has a clearer shape than its current title; duck uses these instead of guessing from the transcript. Put markers in plain message text, never inside a code fence.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_idempotent_and_never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let seed = HandoffSeed {
            id: "r1",
            title: "Fix the bug",
            workflow: "quick-task",
            task: "  fix the bug  ",
            branch: Some("duck/r1"),
            worktree_path: Some("/tmp/wt"),
        };
        let path = seed_handoff_file(dir.path(), &seed);
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.contains("# Handoff — Fix the bug"));
        assert!(first.contains("**Branch:** duck/r1"));
        assert!(first.contains("## Goal\n\nfix the bug\n\n"));

        fs::write(&path, "already resuming").unwrap();
        let path2 = seed_handoff_file(dir.path(), &seed);
        assert_eq!(path, path2);
        assert_eq!(fs::read_to_string(&path).unwrap(), "already resuming");
    }

    #[test]
    fn heartbeat_inserts_under_the_progress_log_header_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let seed = HandoffSeed {
            id: "r1",
            title: "t",
            workflow: "quick-task",
            task: "t",
            branch: None,
            worktree_path: None,
        };
        seed_handoff_file(dir.path(), &seed);
        append_handoff_heartbeat(dir.path(), "r1", "started");
        append_handoff_heartbeat(dir.path(), "r1", "made progress");
        let text = read_handoff(dir.path(), "r1");
        let progress_idx = text.find("## Progress log").unwrap();
        let made_idx = text.find("made progress").unwrap();
        let started_idx = text.find("started").unwrap();
        assert!(progress_idx < made_idx);
        assert!(made_idx < started_idx, "newest heartbeat sorts first");
    }

    #[test]
    fn heartbeat_on_an_unseeded_run_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        append_handoff_heartbeat(dir.path(), "missing", "note");
        assert_eq!(read_handoff(dir.path(), "missing"), "");
    }

    #[test]
    fn progress_excerpt_stops_at_the_next_header_and_caps_line_count() {
        let text = "# Handoff\n\n## Progress log\n\n- line one\n- line two\n- line three\n\n## Resume notes\nshould not appear\n";
        assert_eq!(handoff_progress_excerpt(text, 2), "- line one\n- line two");
        assert_eq!(
            handoff_progress_excerpt(text, 10),
            "- line one\n- line two\n- line three"
        );
        assert_eq!(handoff_progress_excerpt("no marker here", 3), "");
    }

    #[test]
    fn delete_removes_the_file_and_is_a_noop_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let seed = HandoffSeed {
            id: "r1",
            title: "t",
            workflow: "quick-task",
            task: "t",
            branch: None,
            worktree_path: None,
        };
        let path = seed_handoff_file(dir.path(), &seed);
        assert!(path.exists());
        delete_handoff(dir.path(), "r1");
        assert!(!path.exists());
        delete_handoff(dir.path(), "r1"); // no-op
    }
}
