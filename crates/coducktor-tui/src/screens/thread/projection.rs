//! Pure conversation/activity projection for a reduced run history.
//!
//! The reducer remains the compatibility boundary for v1 and v2 durable events. This module
//! deliberately consumes [`ThreadState`] rather than raw JSON so rendering cannot accidentally
//! invent a prompt, treat commentary as a final answer, or lose an orphaned child item.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use coducktor_contract::{ApiRun, RunStatus};
use coducktor_protocol::{MessagePhase, MessageRole, StopReason, ToolStatus, UiItem};

use super::presenters::present_tool;
use super::reducer::{ThreadEntry, ThreadState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    pub id: String,
    pub text: String,
    pub submitted_at: Option<String>,
    pub delivery: DeliveryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Durable,
    Sending,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseView {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Declined,
    Cancelled,
    Interrupted,
    Informational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Commentary,
    Reasoning,
    Tool,
    Question,
    Approval,
    Note,
    Image,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityPresentation {
    pub kind: ActivityKind,
    pub title: String,
    pub subject: Option<String>,
    pub status: ActivityStatus,
    pub preview: Option<String>,
    pub detail: Option<String>,
    pub changed_files: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub presentation: ActivityPresentation,
    pub children: Vec<ActivityNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationStatus {
    Passed,
    Failed,
    NotObserved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Running,
    Completed {
        stop_reason: StopReason,
        verification: VerificationStatus,
    },
    Failed {
        reason: Option<String>,
        verification: VerificationStatus,
    },
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnViewModel {
    pub id: String,
    pub prompt: PromptView,
    pub activity: Vec<ActivityNode>,
    pub response: Option<ResponseView>,
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSummary {
    pub changed_files: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub verification: VerificationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadViewModel {
    pub turns: Vec<TurnViewModel>,
    pub current_status: String,
    pub review_summary: ReviewSummary,
}

impl ThreadViewModel {
    pub fn empty() -> Self {
        Self {
            turns: Vec::new(),
            current_status: "Thinking…".to_owned(),
            review_summary: ReviewSummary {
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
                verification: VerificationStatus::NotObserved,
            },
        }
    }
}

impl Default for ThreadViewModel {
    fn default() -> Self {
        Self::empty()
    }
}

pub fn project_thread(run: &ApiRun, state: &ThreadState) -> ThreadViewModel {
    project_thread_with_root(run, state, None)
}

pub fn project_thread_with_root(
    run: &ApiRun,
    state: &ThreadState,
    project_root: Option<&Path>,
) -> ThreadViewModel {
    let mut turns = Vec::new();
    let mut initial_activity_turn = None;

    // The initial prompt is durable run metadata, not an assistant-generated transcript line.
    // Keep activity that precedes the first v1 user-message in this opening turn.
    if !run.record.task.is_empty() {
        if let Some(first) = state.turns.first()
            && first.user_message.is_none()
        {
            initial_activity_turn = Some(first.id.clone());
            turns.push(build_turn(
                format!("initial:{}", run.record.id),
                PromptView {
                    id: format!("prompt:{}", run.record.id),
                    text: run.record.task.clone(),
                    submitted_at: Some(run.record.created_at.clone()),
                    delivery: DeliveryState::Durable,
                },
                first,
                run.record.status,
                project_root,
            ));
        } else {
            turns.push(build_empty_turn(
                format!("initial:{}", run.record.id),
                PromptView {
                    id: format!("prompt:{}", run.record.id),
                    text: run.record.task.clone(),
                    submitted_at: Some(run.record.created_at.clone()),
                    delivery: DeliveryState::Durable,
                },
                run.record.status,
            ));
        }
    }

    for turn in &state.turns {
        if initial_activity_turn.as_deref() == Some(turn.id.as_str()) {
            continue;
        }
        let Some(message) = &turn.user_message else {
            continue;
        };
        turns.push(build_turn(
            turn.id.clone(),
            PromptView {
                id: format!("prompt:{}", turn.id),
                text: message.text.clone(),
                submitted_at: None,
                delivery: DeliveryState::Durable,
            },
            turn,
            run.record.status,
            project_root,
        ));
    }
    if turns.is_empty() && !run.record.task.is_empty() {
        turns.push(build_empty_turn(
            format!("initial:{}", run.record.id),
            PromptView {
                id: format!("prompt:{}", run.record.id),
                text: run.record.task.clone(),
                submitted_at: Some(run.record.created_at.clone()),
                delivery: DeliveryState::Durable,
            },
            run.record.status,
        ));
    }

    let mut summary = ReviewSummary {
        changed_files: 0,
        added_lines: 0,
        removed_lines: 0,
        verification: VerificationStatus::NotObserved,
    };
    for turn in &turns {
        for node in &turn.activity {
            accumulate_summary(node, &mut summary);
        }
    }
    let current_status = derive_status(run.record.status, turns.last());
    ThreadViewModel {
        turns,
        current_status,
        review_summary: summary,
    }
}

fn build_empty_turn(id: String, prompt: PromptView, status: RunStatus) -> TurnViewModel {
    TurnViewModel {
        id,
        prompt,
        activity: Vec::new(),
        response: None,
        outcome: if matches!(status, RunStatus::Queued | RunStatus::Running) {
            TurnOutcome::Running
        } else {
            TurnOutcome::Unknown
        },
    }
}

fn build_turn(
    id: String,
    prompt: PromptView,
    turn: &super::reducer::ThreadTurn,
    run_status: RunStatus,
    project_root: Option<&Path>,
) -> TurnViewModel {
    let mut roots = Vec::new();
    let mut by_id = BTreeMap::new();
    let mut responses = Vec::new();
    for entry in &turn.items {
        if let ThreadEntry::Item(UiItem::Message(message)) = entry
            && message.role == MessageRole::Assistant
            && is_final_message(message.phase, turn.completed.is_some())
        {
            responses.push(ResponseView {
                id: message.id.clone(),
                text: message.text.clone(),
            });
            continue;
        }
        let (id, parent_id, presentation) = presentation(entry, project_root);
        by_id.insert(
            id.clone(),
            ActivityNode {
                id,
                parent_id,
                presentation,
                children: Vec::new(),
            },
        );
    }
    let ids: Vec<String> = by_id.keys().cloned().collect();
    let known_ids: BTreeSet<String> = ids.iter().cloned().collect();
    let mut pending = BTreeMap::new();
    let mut orphaned = Vec::new();
    for id in ids {
        let Some(node) = by_id.remove(&id) else {
            continue;
        };
        if node.parent_id.is_some()
            && !node
                .parent_id
                .as_deref()
                .is_some_and(|parent_id| known_ids.contains(parent_id))
        {
            orphaned.push(node);
        } else if node.parent_id.is_some() {
            pending.insert(id, node);
        } else {
            roots.push(node);
        }
    }
    for root in &mut roots {
        attach_children(root, &mut pending);
    }
    // A malformed history can contain a cycle. Keep every cyclic/orphaned item visible rather
    // than dropping it while attempting to build the tree.
    while let Some(id) = pending.keys().next().cloned() {
        let Some(mut node) = pending.remove(&id) else {
            continue;
        };
        attach_children(&mut node, &mut pending);
        orphaned.push(node);
    }
    if !orphaned.is_empty() {
        roots.push(ActivityNode {
            id: format!("unlinked:{}", id),
            parent_id: None,
            presentation: ActivityPresentation {
                kind: ActivityKind::Note,
                title: "Unlinked activity".to_owned(),
                subject: None,
                status: ActivityStatus::Informational,
                preview: Some("Items from a partial or legacy stream".to_owned()),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
            children: orphaned,
        });
    }
    let outcome = outcome_for(turn, run_status, &roots);
    TurnViewModel {
        id,
        prompt,
        activity: roots,
        response: responses.pop(),
        outcome,
    }
}

fn attach_children(node: &mut ActivityNode, nodes: &mut BTreeMap<String, ActivityNode>) {
    let child_ids: Vec<String> = nodes
        .iter()
        .filter(|(_, child)| child.parent_id.as_deref() == Some(node.id.as_str()))
        .map(|(id, _)| id.clone())
        .collect();
    for child_id in child_ids {
        let Some(mut child) = nodes.remove(&child_id) else {
            continue;
        };
        attach_children(&mut child, nodes);
        node.children.push(child);
    }
}

fn is_final_message(phase: Option<MessagePhase>, turn_completed: bool) -> bool {
    matches!(phase, Some(MessagePhase::Final)) || phase.is_none() && turn_completed
}

fn presentation(
    entry: &ThreadEntry,
    project_root: Option<&Path>,
) -> (String, Option<String>, ActivityPresentation) {
    match entry {
        ThreadEntry::Item(UiItem::Message(message)) => (
            message.id.clone(),
            message.parent_item_id.clone(),
            ActivityPresentation {
                kind: ActivityKind::Commentary,
                title: "Agent".to_owned(),
                subject: None,
                status: ActivityStatus::Informational,
                preview: Some(message.text.clone()),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
        ThreadEntry::Item(UiItem::Reasoning(reasoning)) => (
            reasoning.id.clone(),
            reasoning.parent_item_id.clone(),
            ActivityPresentation {
                kind: ActivityKind::Reasoning,
                title: "Think".to_owned(),
                subject: None,
                status: ActivityStatus::Informational,
                preview: (!reasoning.text.is_empty()).then_some(reasoning.text.clone()),
                detail: (!reasoning.text.is_empty()).then_some(reasoning.text.clone()),
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
        ThreadEntry::Item(UiItem::Tool(tool)) => {
            let status = match tool.status {
                ToolStatus::Pending => ActivityStatus::Pending,
                ToolStatus::Running => ActivityStatus::Running,
                ToolStatus::Completed => ActivityStatus::Succeeded,
                ToolStatus::Failed => ActivityStatus::Failed,
                ToolStatus::Declined => ActivityStatus::Declined,
            };
            let semantic = present_tool(tool, project_root);
            (
                tool.id.clone(),
                tool.parent_item_id.clone(),
                ActivityPresentation {
                    kind: ActivityKind::Tool,
                    title: semantic.title,
                    subject: semantic.subject,
                    status,
                    preview: semantic.preview,
                    detail: semantic.detail,
                    changed_files: semantic.changed_files,
                    added_lines: semantic.added_lines,
                    removed_lines: semantic.removed_lines,
                },
            )
        }
        ThreadEntry::Ask(ask) => (
            ask.id.clone(),
            None,
            ActivityPresentation {
                kind: ActivityKind::Question,
                title: "Waiting for your answer".to_owned(),
                subject: ask
                    .questions
                    .first()
                    .map(|question| question.question.clone()),
                status: if ask.resolved {
                    ActivityStatus::Succeeded
                } else {
                    ActivityStatus::Pending
                },
                preview: ask.answer.clone(),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
        ThreadEntry::Note(note) => (
            note.id.clone(),
            None,
            ActivityPresentation {
                kind: ActivityKind::Note,
                title: "Note".to_owned(),
                subject: None,
                status: if matches!(note.tone, super::reducer::NoteTone::Danger) {
                    ActivityStatus::Failed
                } else {
                    ActivityStatus::Informational
                },
                preview: Some(note.text.clone()),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
        ThreadEntry::Image(image) => (
            image.id.clone(),
            None,
            ActivityPresentation {
                kind: ActivityKind::Image,
                title: "Image".to_owned(),
                subject: image.name.clone(),
                status: ActivityStatus::Informational,
                preview: Some(image.url.clone()),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
        ThreadEntry::ProviderAuthRequired(card) => (
            card.id.clone(),
            None,
            ActivityPresentation {
                kind: ActivityKind::Approval,
                title: "Permission required".to_owned(),
                subject: Some(format!("{:?}", card.provider)),
                status: ActivityStatus::Pending,
                preview: Some(format!("incident {}", card.auth_failure_id)),
                detail: None,
                changed_files: 0,
                added_lines: 0,
                removed_lines: 0,
            },
        ),
    }
}

fn outcome_for(
    turn: &super::reducer::ThreadTurn,
    run_status: RunStatus,
    roots: &[ActivityNode],
) -> TurnOutcome {
    let verification = verification_status(roots);
    if let Some(completed) = &turn.completed {
        return match completed.stop_reason {
            StopReason::Cancelled => TurnOutcome::Interrupted,
            StopReason::Error | StopReason::Refusal | StopReason::Timeout => TurnOutcome::Failed {
                reason: None,
                verification,
            },
            reason => TurnOutcome::Completed {
                stop_reason: reason,
                verification,
            },
        };
    }
    match run_status {
        RunStatus::Queued | RunStatus::Running | RunStatus::Waiting => TurnOutcome::Running,
        RunStatus::Cancelled => TurnOutcome::Interrupted,
        RunStatus::Failed => TurnOutcome::Failed {
            reason: None,
            verification,
        },
        _ => TurnOutcome::Unknown,
    }
}

fn verification_status(nodes: &[ActivityNode]) -> VerificationStatus {
    let mut observed = false;
    for node in nodes {
        if node.presentation.kind == ActivityKind::Tool
            && (node.presentation.title.contains("Tests")
                || node.presentation.title.contains("cargo")
                || node.presentation.title.contains("execute"))
        {
            observed = true;
            if node.presentation.status == ActivityStatus::Failed {
                return VerificationStatus::Failed;
            }
        }
        match verification_status(&node.children) {
            VerificationStatus::Failed => return VerificationStatus::Failed,
            VerificationStatus::Passed => observed = true,
            VerificationStatus::NotObserved => {}
        }
    }
    if observed {
        VerificationStatus::Passed
    } else {
        VerificationStatus::NotObserved
    }
}

fn accumulate_summary(node: &ActivityNode, summary: &mut ReviewSummary) {
    summary.changed_files += node.presentation.changed_files;
    summary.added_lines += node.presentation.added_lines;
    summary.removed_lines += node.presentation.removed_lines;
    if node.presentation.status == ActivityStatus::Failed
        && node.presentation.kind == ActivityKind::Tool
    {
        summary.verification = VerificationStatus::Failed;
    }
    if summary.verification == VerificationStatus::NotObserved
        && node.presentation.kind == ActivityKind::Tool
        && node.presentation.title.contains("Tests")
        && node.presentation.status == ActivityStatus::Succeeded
    {
        summary.verification = VerificationStatus::Passed;
    }
    for child in &node.children {
        accumulate_summary(child, summary);
    }
}

fn derive_status(status: RunStatus, turn: Option<&TurnViewModel>) -> String {
    if let Some(turn) = turn {
        if let Some(node) = running_node(&turn.activity) {
            return node.presentation.title.clone();
        }
        if turn.activity.iter().any(|node| {
            node.presentation.kind == ActivityKind::Question
                && node.presentation.status == ActivityStatus::Pending
        }) {
            return "Waiting for your answer".to_owned();
        }
        if let Some(response) = &turn.response
            && !response.text.is_empty()
        {
            return "Completed".to_owned();
        }
    }
    match status {
        RunStatus::Queued => "Queued".to_owned(),
        RunStatus::Running => "Thinking…".to_owned(),
        RunStatus::Waiting => "Waiting for your answer".to_owned(),
        RunStatus::Review => "Waiting for review".to_owned(),
        RunStatus::Done => "Completed".to_owned(),
        RunStatus::Failed => "Failed".to_owned(),
        RunStatus::Cancelled => "Cancelled".to_owned(),
    }
}

fn running_node(nodes: &[ActivityNode]) -> Option<&ActivityNode> {
    nodes.iter().find_map(|node| {
        (node.presentation.status == ActivityStatus::Running)
            .then_some(node)
            .or_else(|| running_node(&node.children))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::{RunEvent, RunRecord};
    use serde_json::json;

    fn run(task: &str, status: RunStatus) -> ApiRun {
        ApiRun {
            record: RunRecord {
                id: "run-1".to_owned(),
                task: task.to_owned(),
                status,
                created_at: "2026-08-17T00:00:00Z".to_owned(),
                ..RunRecord::default()
            },
            usage: None,
        }
    }

    fn event(seq: f64, event_type: &str, extra: serde_json::Value) -> RunEvent {
        RunEvent {
            seq,
            ts: "2026-08-17T00:00:00Z".to_owned(),
            step_id: None,
            event_type: event_type.to_owned(),
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn initial_and_follow_up_prompts_are_exact_and_distinct() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "follow\nup"})),
            event(2.0, "turn.completed", json!({"stopReason": "end_turn"})),
        ];
        let state = super::super::reducer::reduce_thread(&events, Default::default());
        let view = project_thread(&run("initial  exact", RunStatus::Done), &state);
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].prompt.text, "initial  exact");
        assert_eq!(view.turns[1].prompt.text, "follow\nup");
    }

    #[test]
    fn commentary_and_final_messages_have_different_roles_in_the_projection() {
        let events = vec![
            event(
                1.0,
                "item.started",
                json!({"item": {"kind":"message","id":"c","role":"assistant","text":"working","phase":"commentary"}}),
            ),
            event(
                2.0,
                "item.completed",
                json!({"item": {"kind":"message","id":"f","role":"assistant","text":"done","phase":"final"}}),
            ),
            event(3.0, "turn.completed", json!({"stopReason":"end_turn"})),
        ];
        let state = super::super::reducer::reduce_thread(&events, Default::default());
        let view = project_thread(&run("task", RunStatus::Done), &state);
        assert_eq!(
            view.turns[0].response.as_ref().map(|r| r.text.as_str()),
            Some("done")
        );
        assert_eq!(
            view.turns[0].activity[0].presentation.kind,
            ActivityKind::Commentary
        );
    }

    #[test]
    fn failed_execute_is_observed_as_failed_verification() {
        let events = vec![event(
            1.0,
            "item.completed",
            json!({"item": {"kind":"tool","id":"x","name":"exec","toolKind":"execute","title":"Tests","status":"failed","exitCode":1}}),
        )];
        let state = super::super::reducer::reduce_thread(&events, Default::default());
        let view = project_thread(&run("task", RunStatus::Failed), &state);
        assert_eq!(view.review_summary.verification, VerificationStatus::Failed);
    }

    #[test]
    fn orphaned_activity_is_retained_under_an_explicit_group() {
        let events = vec![event(
            1.0,
            "item.started",
            json!({
                "item": {
                    "kind": "tool",
                    "id": "orphan",
                    "name": "Read",
                    "toolKind": "read",
                    "title": "Read missing-parent",
                    "status": "running",
                    "parentItemId": "missing-parent"
                }
            }),
        )];
        let state = super::super::reducer::reduce_thread(&events, Default::default());
        let view = project_thread(&run("task", RunStatus::Running), &state);
        assert_eq!(
            view.turns[0].activity[0].presentation.title,
            "Unlinked activity"
        );
        assert_eq!(view.turns[0].activity[0].children[0].id, "orphan");
    }
}
