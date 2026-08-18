//! Backend-neutral session policy and the injected session seam.

pub use super::{
    AgentSession, CheckExecutor, CheckResult, ContinueOptions, ContinueResult, DiffInspector,
    RuntimeOptions, SessionFactory, SessionOutcome, SessionReport, SessionRequest,
};
use crate::runs::task_markers::canonicalize_markers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnMarkerDecision {
    Closed,
    Done,
    Ask,
    Monitoring,
    Waiting,
    AutonomousContinue,
}

/// Enough retries to recover from a premature turn boundary without letting a malformed
/// backend churn through dozens of paid turns.
pub const MAX_AUTONOMOUS_CONTINUES: u32 = 4;

fn is_standalone_legacy_done(text: &str) -> bool {
    matches!(
        text.trim_end().lines().next_back().map(str::trim),
        Some("DONE" | "[DONE]")
    )
}

fn trailing_marker(text: &str, marker: &str) -> bool {
    let canonical = canonicalize_markers(text);
    canonical.trim_end().ends_with(&format!("DUCK:{marker}"))
        || (marker == "DONE" && is_standalone_legacy_done(&canonical))
}

pub fn append_turn_text(current: &str, next: &str) -> String {
    if current.is_empty() {
        return next.to_owned();
    }
    if next.is_empty() {
        return current.to_owned();
    }
    format!("{current}\n{next}")
}

/// Remove a complete trailing lifecycle marker from display text. Delta-oriented backends may
/// split a marker across events; callers should pass the accumulated turn, just as they do to
/// [`decide_turn_marker`], so a complete marker is never rendered as transcript prose.
pub fn strip_turn_marker(text: &str) -> String {
    let canonical = canonicalize_markers(text);
    let trimmed = canonical.trim_end();
    for marker in ["DONE", "MONITORING"] {
        let suffix = format!("DUCK:{marker}");
        if let Some(without_marker) = trimmed.strip_suffix(&suffix) {
            return without_marker.trim_end().to_owned();
        }
    }
    if is_standalone_legacy_done(&canonical) {
        let mut lines = canonical.trim_end().lines().collect::<Vec<_>>();
        lines.pop();
        return lines.join("\n").trim_end().to_owned();
    }
    canonical
}

pub fn decide_turn_marker(
    turn_text: &str,
    session_open: bool,
    valid_ask: bool,
) -> TurnMarkerDecision {
    if !session_open {
        return TurnMarkerDecision::Closed;
    }
    if trailing_marker(turn_text, "DONE") {
        return TurnMarkerDecision::Done;
    }
    if valid_ask {
        return TurnMarkerDecision::Ask;
    }
    if trailing_marker(turn_text, "MONITORING") {
        return TurnMarkerDecision::Monitoring;
    }
    TurnMarkerDecision::Waiting
}

/// Turn-end precedence with the autonomous nudge inserted below `DONE`, `ASK`, and monitoring.
pub fn autonomous_turn_decision(
    turn_text: &str,
    session_open: bool,
    autonomous: bool,
    continues: u32,
    max_continues: u32,
    valid_ask: bool,
) -> TurnMarkerDecision {
    let decision = decide_turn_marker(turn_text, session_open, valid_ask);
    if autonomous && decision == TurnMarkerDecision::Waiting && continues < max_continues {
        TurnMarkerDecision::AutonomousContinue
    } else {
        decision
    }
}

pub fn monitoring_is_non_attention(decision: TurnMarkerDecision) -> bool {
    decision == TurnMarkerDecision::Monitoring
}

pub fn ask_wins_over_monitoring(decision: TurnMarkerDecision) -> bool {
    decision == TurnMarkerDecision::Ask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_decisions_keep_done_ask_monitoring_precedence() {
        assert_eq!(
            autonomous_turn_decision("DUCK:DONE", true, true, 0, 4, true),
            TurnMarkerDecision::Done
        );
        assert_eq!(
            autonomous_turn_decision("DUCK:MONITORING", true, false, 0, 4, true),
            TurnMarkerDecision::Ask
        );
        assert_eq!(
            autonomous_turn_decision("DUCK:MONITORING", true, false, 0, 4, false),
            TurnMarkerDecision::Monitoring
        );
    }

    #[test]
    fn autonomous_nudges_are_bounded_and_closed_sessions_do_not_nudge() {
        assert_eq!(
            autonomous_turn_decision("progress", true, true, 0, 1, false),
            TurnMarkerDecision::AutonomousContinue
        );
        assert_eq!(
            autonomous_turn_decision("progress", true, true, 1, 1, false),
            TurnMarkerDecision::Waiting
        );
        assert_eq!(
            autonomous_turn_decision("progress", false, true, 0, 1, false),
            TurnMarkerDecision::Closed
        );
    }

    #[test]
    fn complete_lifecycle_markers_are_hidden_from_transcript_text() {
        assert_eq!(strip_turn_marker("progress\nDUCK:DONE\n"), "progress");
        assert_eq!(
            strip_turn_marker("still working DUCK:MONITORING"),
            "still working"
        );
        assert_eq!(
            strip_turn_marker("mentioning DUCK:DONE in prose"),
            "mentioning DUCK:DONE in prose"
        );
        let legacy = format!("still working {}:MONITORING", concat!("C", "E", "Z"));
        assert_eq!(strip_turn_marker(&legacy), "still working");
        assert_eq!(
            strip_turn_marker("all checks pass\nDONE\n"),
            "all checks pass"
        );
        assert_eq!(strip_turn_marker("DONE"), "");
        assert_eq!(
            strip_turn_marker("this is DONE in prose"),
            "this is DONE in prose"
        );
        assert_eq!(
            decide_turn_marker("work complete\nDONE", true, false),
            TurnMarkerDecision::Done
        );
    }
}
