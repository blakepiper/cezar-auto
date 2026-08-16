//! The run header's action policy — WHICH actions a run offers, as a pure function of the
//! record. Ports `packages/web/src/routes/task-thread/run-actions.ts` verbatim so the two
//! cockpits agree; every rule lives here so a table test can pin it per status.

use coducktor_contract::{RunRecord, RunStatus, Runner};

use crate::screens::runs_util::{can_be_unread, is_unread};

/// "Active" in the legacy sense: the engine still owns the run (a queued run is parked but
/// claimed). `Review` is deliberately NOT active — a parked review can be continued, archived
/// or deleted like any finished run.
pub fn is_run_active(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Running | RunStatus::Queued | RunStatus::Waiting
    )
}

/// The latest agent session across steps — what Continue/Terminal resume.
pub fn last_session_id(run: &RunRecord) -> Option<&str> {
    run.steps
        .iter()
        .rev()
        .find_map(|step| step.session_id.as_deref())
}

/// Mirrors the server's `SAFE_SESSION_ID` (server.ts, #431) — kept in lockstep by hand.
fn is_safe_session_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if id.len() > 200 {
        return false;
    }
    let first_ok = first.is_ascii_alphanumeric() || first == '.' || first == '_';
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// The per-backend take-over command — mirrors the server's `resumeCommand`. Records without
/// a runner predate the runner choice and default to Claude. `None` for an id the server would
/// refuse (fails closed rather than offering a shell splice of an untrusted string).
pub fn resume_command(runner: Option<Runner>, session_id: &str) -> Option<String> {
    if !is_safe_session_id(session_id) {
        return None;
    }
    Some(match runner {
        Some(Runner::Codex) => format!("codex resume {session_id}"),
        Some(Runner::OpenCode) => format!("opencode --session {session_id}"),
        _ => format!("claude --resume {session_id}"),
    })
}

/// The copyable "take over interactively" line under the header — only once the engine has
/// let go of the session (same gate as the Terminal button).
pub fn resume_hint(run: &RunRecord) -> Option<String> {
    if is_run_active(run.status) {
        return None;
    }
    let session_id = last_session_id(run)?;
    let command = resume_command(run.runner, session_id)?;
    Some(match &run.worktree_path {
        Some(path) => format!("cd {path} && {command}"),
        None => command,
    })
}

/// The run header's action policy — which actions a run offers, as a pure function of status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunActionFlags {
    /// waiting → close the session; review → accept the changes without a PR.
    pub finish: bool,
    /// Reopen the last agent session in-process.
    pub continue_run: bool,
    /// Hand the session to a real terminal (open-in-cli).
    pub terminal: bool,
    /// Archive when live, unarchive when archived.
    pub archive: bool,
    /// Put a read, finished task back into the unread list.
    pub mark_unread: bool,
    /// Stop an active run. Mutually exclusive with delete, by construction.
    pub cancel: bool,
    /// Remove the run, its transcript, worktree and branch. Terminal runs only.
    pub delete_run: bool,
}

pub fn run_action_flags(run: &coducktor_contract::ApiRun) -> RunActionFlags {
    let active = is_run_active(run.record.status);
    let has_session = last_session_id(&run.record).is_some();
    RunActionFlags {
        finish: matches!(run.record.status, RunStatus::Waiting | RunStatus::Review),
        continue_run: !active && has_session,
        terminal: !active && has_session,
        archive: !active,
        mark_unread: can_be_unread(run) && !is_unread(run),
        cancel: active,
        delete_run: !active,
    }
}

/// The Finish button's tooltip — review-gate accept reads differently from closing a session.
pub fn finish_title(status: RunStatus) -> &'static str {
    if status == RunStatus::Review {
        "Accept the changes without a PR"
    } else {
        "Close the session"
    }
}

/// 1-based position among the queued, unarchived runs, FIFO by `createdAt`. `None` when the
/// run is not itself queued.
pub fn queue_position(runs: &[RunRecord], run_id: &str) -> Option<usize> {
    let mut queued: Vec<&RunRecord> = runs
        .iter()
        .filter(|run| !run.archived && run.status == RunStatus::Queued)
        .collect();
    queued.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    queued
        .iter()
        .position(|run| run.id == run_id)
        .map(|index| index + 1)
}

/// Which seam an AskUser answer (or the composer's Continue) travels on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// The engine still owns a session (or the run has not started yet): a live message.
    Live,
    /// The session ended while unanswered — deliver as the opening prompt of `continue`.
    Resume,
    /// The run is closed and never recorded a session — nothing to reopen.
    Unavailable,
}

/// The delivery route for one ask answer, as a pure function of the run record — mirrors the
/// composer's own routing: a live/queued run sends, a closed one with a session continues.
pub fn ask_delivery_mode(run: &coducktor_contract::ApiRun) -> DeliveryMode {
    if is_run_active(run.record.status) {
        DeliveryMode::Live
    } else if run_action_flags(run).continue_run {
        DeliveryMode::Resume
    } else {
        DeliveryMode::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::{ApiRun, RunRecord, StepKind, StepState, StepStatus};

    fn run(status: RunStatus, archived: bool, session_id: Option<&str>) -> ApiRun {
        let steps = session_id
            .map(|id| {
                vec![StepState {
                    id: "s1".to_owned(),
                    name: "agent".to_owned(),
                    kind: StepKind::Agent,
                    status: StepStatus::Done,
                    iterations: 1.0,
                    tokens_used: 0.0,
                    input_tokens: None,
                    output_tokens: None,
                    usage_invocations_started: None,
                    usage_invocations_observed: None,
                    usage_turns_started: None,
                    usage_turns_recorded: None,
                    usage_invocation_epoch: None,
                    started_at: None,
                    finished_at: None,
                    error: None,
                    session_id: Some(id.to_owned()),
                    backend: None,
                    requested_runner: None,
                    profile_id: None,
                    reasoning_effort: None,
                    cost_usd: None,
                    model_identity: None,
                }]
            })
            .unwrap_or_default();
        ApiRun {
            record: RunRecord {
                id: "r1".to_owned(),
                status,
                archived,
                steps,
                created_at: "2026-08-15T00:00:00Z".to_owned(),
                ..RunRecord::default()
            },
            usage: None,
        }
    }

    #[test]
    fn review_is_not_active_but_running_and_queued_and_waiting_are() {
        assert!(!is_run_active(RunStatus::Review));
        assert!(is_run_active(RunStatus::Running));
        assert!(is_run_active(RunStatus::Queued));
        assert!(is_run_active(RunStatus::Waiting));
        assert!(!is_run_active(RunStatus::Done));
    }

    #[test]
    fn finish_offered_only_at_waiting_and_review() {
        for status in [RunStatus::Waiting, RunStatus::Review] {
            assert!(run_action_flags(&run(status, false, None)).finish);
        }
        for status in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Done,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert!(!run_action_flags(&run(status, false, None)).finish);
        }
    }

    #[test]
    fn continue_and_terminal_need_a_closed_run_with_a_session() {
        let closed_with_session = run(RunStatus::Done, false, Some("abc123"));
        let flags = run_action_flags(&closed_with_session);
        assert!(flags.continue_run);
        assert!(flags.terminal);

        let closed_without_session = run(RunStatus::Done, false, None);
        let flags = run_action_flags(&closed_without_session);
        assert!(!flags.continue_run);
        assert!(!flags.terminal);

        let active_with_session = run(RunStatus::Running, false, Some("abc123"));
        let flags = run_action_flags(&active_with_session);
        assert!(!flags.continue_run);
        assert!(!flags.terminal);
    }

    #[test]
    fn cancel_and_delete_are_exact_complements_of_active() {
        for status in [RunStatus::Queued, RunStatus::Running, RunStatus::Waiting] {
            let flags = run_action_flags(&run(status, false, None));
            assert!(flags.cancel);
            assert!(!flags.delete_run);
        }
        for status in [
            RunStatus::Review,
            RunStatus::Done,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            let flags = run_action_flags(&run(status, false, None));
            assert!(!flags.cancel);
            assert!(flags.delete_run);
        }
    }

    #[test]
    fn resume_command_fails_closed_on_an_unsafe_session_id() {
        assert_eq!(
            resume_command(None, "abc-123.ok"),
            Some("claude --resume abc-123.ok".to_owned())
        );
        assert_eq!(
            resume_command(Some(Runner::Codex), "abc"),
            Some("codex resume abc".to_owned())
        );
        assert_eq!(resume_command(None, "../../etc/passwd"), None);
        assert_eq!(resume_command(None, "rm -rf ~"), None);
    }

    #[test]
    fn resume_hint_is_absent_while_active_and_prefixes_the_worktree_cd() {
        let mut active = run(RunStatus::Running, false, Some("abc"));
        assert_eq!(resume_hint(&active.record), None);

        let mut closed = run(RunStatus::Done, false, Some("abc"));
        closed.record.worktree_path = Some("/tmp/wt".to_owned());
        assert_eq!(
            resume_hint(&closed.record),
            Some("cd /tmp/wt && claude --resume abc".to_owned())
        );

        active.record.status = RunStatus::Done;
        assert!(resume_hint(&active.record).is_some());
    }

    #[test]
    fn finish_title_reads_differently_for_review() {
        assert_eq!(
            finish_title(RunStatus::Review),
            "Accept the changes without a PR"
        );
        assert_eq!(finish_title(RunStatus::Waiting), "Close the session");
    }

    #[test]
    fn queue_position_is_one_based_fifo_among_unarchived_queued_runs() {
        let mut a = run(RunStatus::Queued, false, None).record;
        a.id = "a".to_owned();
        a.created_at = "2026-08-15T00:00:00Z".to_owned();
        let mut b = run(RunStatus::Queued, false, None).record;
        b.id = "b".to_owned();
        b.created_at = "2026-08-15T00:00:01Z".to_owned();
        let mut archived = run(RunStatus::Queued, true, None).record;
        archived.id = "c".to_owned();
        archived.created_at = "2026-08-15T00:00:02Z".to_owned();
        let runs = vec![b.clone(), a.clone(), archived];
        assert_eq!(queue_position(&runs, "a"), Some(1));
        assert_eq!(queue_position(&runs, "b"), Some(2));
        assert_eq!(queue_position(&runs, "c"), None);
        assert_eq!(queue_position(&runs, "missing"), None);
    }

    #[test]
    fn ask_delivery_mode_follows_the_live_resume_unavailable_ladder() {
        assert_eq!(
            ask_delivery_mode(&run(RunStatus::Running, false, None)),
            DeliveryMode::Live
        );
        assert_eq!(
            ask_delivery_mode(&run(RunStatus::Done, false, Some("abc"))),
            DeliveryMode::Resume
        );
        assert_eq!(
            ask_delivery_mode(&run(RunStatus::Done, false, None)),
            DeliveryMode::Unavailable
        );
    }
}
