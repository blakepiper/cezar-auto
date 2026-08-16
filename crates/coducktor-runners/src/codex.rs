//! Codex app-server JSON-RPC to protocol-v2 mapping.
//!
//! The input is intentionally structural JSON. App-server frames are external,
//! versioned independently of this crate, and malformed frames are a normal
//! degradation path rather than a reason to fail a run.

use std::collections::{BTreeMap, BTreeSet};

use coducktor_protocol::{
    FileDiff, MessagePhase, MessageRole, PlanEntry, PlanStatus, StopReason, TokenUsage, ToolKind,
    ToolLocation, ToolStatus, UiBackend, UiEvent, UiItem, UiMessageItem, UiReasoningItem,
    UiToolItem, tool_display,
};
use serde_json::{Map, Value, json};

use crate::wire::{as_finite_number, as_nonempty_str, as_record, json_string};

/// Accumulated reasoning channels. Codex streams raw and summary reasoning separately.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReasoningAccumulator {
    pub text: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexCollabTask {
    item_id: String,
    prompt: Option<String>,
    model: Option<String>,
}

/// Explicit immutable state carried between Codex frames.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexUiMapperState {
    pub session_started: bool,
    pub turn_seq: u64,
    pub current_turn_id: Option<String>,
    pub pending_turn_usage: Option<TokenUsage>,
    pub known_items: BTreeSet<String>,
    pub outputs: BTreeMap<String, String>,
    pub reasonings: BTreeMap<String, ReasoningAccumulator>,
    pub plan_from_notification: bool,
    pub review_item_id: Option<String>,
    collab_tasks: BTreeMap<String, CodexCollabTask>,
}

/// The result of folding one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexUiMapping {
    pub events: Vec<UiEvent>,
    pub state: CodexUiMapperState,
}

/// Creates an empty mapper state.
pub fn create_codex_ui_state() -> CodexUiMapperState {
    CodexUiMapperState {
        session_started: false,
        turn_seq: 0,
        current_turn_id: None,
        pending_turn_usage: None,
        known_items: BTreeSet::new(),
        outputs: BTreeMap::new(),
        reasonings: BTreeMap::new(),
        plan_from_notification: false,
        review_item_id: None,
        collab_tasks: BTreeMap::new(),
    }
}

/// Emits session.started for the out-of-band thread/start result.
pub fn codex_session_started(thread_id: &str, state: &CodexUiMapperState) -> CodexUiMapping {
    if state.session_started || thread_id.is_empty() {
        return no_events(state);
    }
    let mut next = state.clone();
    next.session_started = true;
    CodexUiMapping {
        events: vec![UiEvent::SessionStarted {
            session_id: thread_id.to_owned(),
            backend: UiBackend::Codex,
            model: None,
            cwd: None,
            tools: None,
        }],
        state: next,
    }
}

/// Folds one parsed Codex JSON-RPC frame. Responses and server requests belong
/// to the transport layer and are not mapper notifications.
pub fn map_codex_notification(value: &Value, state: &CodexUiMapperState) -> CodexUiMapping {
    let Some(frame) = as_record(value) else {
        return no_events(state);
    };
    if frame.get("method").and_then(Value::as_str).is_none() || frame.contains_key("id") {
        return no_events(state);
    }
    let method = frame
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = frame.get("params").and_then(as_record);
    match method {
        "thread/started" => {
            codex_session_started(params.and_then(thread_id_of).unwrap_or_default(), state)
        }
        "turn/started" => map_turn_started(params.unwrap_or(&Map::new()), state),
        "turn/completed" => map_turn_end(params.unwrap_or(&Map::new()), state, false),
        "turn/failed" => map_turn_end(params.unwrap_or(&Map::new()), state, true),
        "turn/plan/updated" => map_turn_plan_updated(params.unwrap_or(&Map::new()), state),
        "item/started" => {
            map_item_lifecycle(params.unwrap_or(&Map::new()), state, ItemEventType::Started)
        }
        "item/updated" => {
            map_item_lifecycle(params.unwrap_or(&Map::new()), state, ItemEventType::Updated)
        }
        "item/completed" => map_item_lifecycle(
            params.unwrap_or(&Map::new()),
            state,
            ItemEventType::Completed,
        ),
        "item/agentMessage/delta" => map_delta(
            params.unwrap_or(&Map::new()),
            state,
            DeltaField::Text,
            ReasoningChannel::Text,
        ),
        "item/reasoning/textDelta" => map_delta(
            params.unwrap_or(&Map::new()),
            state,
            DeltaField::Reasoning,
            ReasoningChannel::Text,
        ),
        "item/reasoning/summaryDelta" | "item/reasoning/summaryTextDelta" => map_delta(
            params.unwrap_or(&Map::new()),
            state,
            DeltaField::Reasoning,
            ReasoningChannel::Summary,
        ),
        "item/commandExecution/outputDelta" => map_delta(
            params.unwrap_or(&Map::new()),
            state,
            DeltaField::Output,
            ReasoningChannel::Text,
        ),
        "thread/tokenUsage/updated" => map_token_usage(params.unwrap_or(&Map::new()), state),
        _ => no_events(state),
    }
}

fn no_events(state: &CodexUiMapperState) -> CodexUiMapping {
    CodexUiMapping {
        events: Vec::new(),
        state: state.clone(),
    }
}

fn map_turn_started(params: &Map<String, Value>, state: &CodexUiMapperState) -> CodexUiMapping {
    let turn_seq = state.turn_seq + 1;
    let turn_id = turn_id_of(params)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("turn_{turn_seq}"));
    let mut next = state.clone();
    next.turn_seq = turn_seq;
    next.current_turn_id = Some(turn_id.clone());
    next.pending_turn_usage = None;
    next.plan_from_notification = false;
    next.reasonings = BTreeMap::new();
    CodexUiMapping {
        events: vec![UiEvent::TurnStarted { turn_id }],
        state: next,
    }
}

fn map_turn_end(
    params: &Map<String, Value>,
    state: &CodexUiMapperState,
    failed: bool,
) -> CodexUiMapping {
    let mut turn_seq = state.turn_seq;
    let turn_id = turn_id_of(params)
        .map(ToOwned::to_owned)
        .or_else(|| state.current_turn_id.clone())
        .unwrap_or_else(|| {
            turn_seq += 1;
            format!("turn_{turn_seq}")
        });
    let closes_active_turn = state.current_turn_id.is_none()
        || state.current_turn_id.as_deref() == Some(turn_id.as_str());
    let mut events = Vec::new();
    if closes_active_turn && let Some(review_id) = &state.review_item_id {
        events.push(UiEvent::ItemCompleted {
            item: UiItem::Tool(review_item(
                review_id,
                "enteredReviewMode",
                if failed {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                },
            )),
        });
    }
    let usage = if state.current_turn_id.as_deref() == Some(turn_id.as_str()) {
        state.pending_turn_usage.clone()
    } else {
        None
    };
    events.push(UiEvent::TurnCompleted {
        turn_id,
        stop_reason: turn_stop_reason(params, failed),
        usage,
        cost_usd: None,
    });
    let mut next = state.clone();
    next.turn_seq = turn_seq;
    if closes_active_turn {
        next.current_turn_id = None;
        next.pending_turn_usage = None;
        next.review_item_id = None;
    }
    CodexUiMapping {
        events,
        state: next,
    }
}

fn turn_stop_reason(params: &Map<String, Value>, failed: bool) -> StopReason {
    if params
        .get("turn")
        .and_then(as_record)
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        == Some("interrupted")
    {
        return StopReason::Cancelled;
    }
    if !failed {
        return StopReason::EndTurn;
    }
    if error_message(params.get("error"))
        .is_some_and(|message| message.to_ascii_lowercase().contains("interrupt"))
    {
        StopReason::Cancelled
    } else {
        StopReason::Error
    }
}

fn map_turn_plan_updated(
    params: &Map<String, Value>,
    state: &CodexUiMapperState,
) -> CodexUiMapping {
    let Some(entries) = turn_plan_entries(params.get("plan")) else {
        return no_events(state);
    };
    let mut next = state.clone();
    next.plan_from_notification = true;
    CodexUiMapping {
        events: vec![UiEvent::PlanUpdated { entries }],
        state: next,
    }
}

fn turn_plan_entries(value: Option<&Value>) -> Option<Vec<PlanEntry>> {
    let plan = value?.as_array()?;
    let entries: Vec<PlanEntry> = plan
        .iter()
        .filter_map(|step| {
            let step = as_record(step)?;
            let content = as_nonempty_str(step.get("step"))?;
            Some(PlanEntry {
                content: content.to_owned(),
                status: turn_plan_status(step.get("status")),
                priority: None,
                active_form: None,
            })
        })
        .collect();
    (plan.is_empty() || !entries.is_empty()).then_some(entries)
}

fn turn_plan_status(value: Option<&Value>) -> PlanStatus {
    match value.and_then(Value::as_str) {
        Some("completed") => PlanStatus::Completed,
        Some("inProgress") | Some("in_progress") => PlanStatus::InProgress,
        _ => PlanStatus::Pending,
    }
}

#[derive(Debug, Clone, Copy)]
enum ItemEventType {
    Started,
    Updated,
    Completed,
}

impl ItemEventType {
    fn event(self, item: UiItem) -> UiEvent {
        match self {
            Self::Started => UiEvent::ItemStarted { item },
            Self::Updated => UiEvent::ItemUpdated { item },
            Self::Completed => UiEvent::ItemCompleted { item },
        }
    }

    fn is_completed(self) -> bool {
        matches!(self, Self::Completed)
    }
}

fn map_item_lifecycle(
    params: &Map<String, Value>,
    state: &CodexUiMapperState,
    event_type: ItemEventType,
) -> CodexUiMapping {
    let Some(raw) = params.get("item").and_then(as_record) else {
        return no_events(state);
    };
    let Some(kind) = raw.get("type").and_then(Value::as_str) else {
        return no_events(state);
    };
    if matches!(kind, "todoList" | "todo_list" | "plan") {
        if state.plan_from_notification {
            return no_events(state);
        }
        let Some(entries) = plan_entries_of(raw) else {
            return no_events(state);
        };
        return CodexUiMapping {
            events: vec![UiEvent::PlanUpdated { entries }],
            state: state.clone(),
        };
    }
    if kind == "userMessage" {
        return no_events(state);
    }
    let Some(id) = as_nonempty_str(raw.get("id")) else {
        return no_events(state);
    };
    if matches!(kind, "enteredReviewMode" | "exitedReviewMode") {
        return map_review_mode(raw, id, kind, event_type, state);
    }
    if kind == "subAgentActivity" {
        return map_sub_agent_activity(raw, id, state);
    }
    if matches!(kind, "collabAgentToolCall" | "collabToolCall") {
        return map_collab_tool_call(raw, id, event_type, state);
    }

    let mut item = match kind {
        "agentMessage" => UiItem::Message(message_item(raw, id)),
        "reasoning" => UiItem::Reasoning(reasoning_item(raw, id, state)),
        _ => UiItem::Tool(tool_item(raw, id, kind, event_type, state)),
    };
    if let Some(parent_item_id) = collab_parent_item_id(params, state) {
        set_parent(&mut item, parent_item_id);
    }
    let events = vec![event_type.event(item)];
    let mut next = state.clone();
    if event_type.is_completed() {
        if !state.known_items.contains(id)
            && !state.outputs.contains_key(id)
            && !state.reasonings.contains_key(id)
        {
            return CodexUiMapping {
                events,
                state: next,
            };
        }
        next.known_items.remove(id);
        next.outputs.remove(id);
        next.reasonings.remove(id);
    } else if !state.known_items.contains(id) {
        next.known_items.insert(id.to_owned());
    }
    CodexUiMapping {
        events,
        state: next,
    }
}

fn message_item(raw: &Map<String, Value>, id: &str) -> UiMessageItem {
    UiMessageItem {
        id: id.to_owned(),
        role: MessageRole::Assistant,
        text: raw
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        phase: match raw.get("phase").and_then(Value::as_str) {
            Some("commentary") => Some(MessagePhase::Commentary),
            Some("final_answer") => Some(MessagePhase::Final),
            _ => None,
        },
        parent_item_id: None,
    }
}

fn reasoning_item(
    raw: &Map<String, Value>,
    id: &str,
    state: &CodexUiMapperState,
) -> UiReasoningItem {
    let streamed = state.reasonings.get(id);
    let summary = longer(
        reasoning_snapshot_text(raw.get("summary")),
        streamed.map(|value| value.summary.as_str()),
    );
    UiReasoningItem {
        id: id.to_owned(),
        text: reasoning_snapshot_text(raw.get("content"))
            .or_else(|| streamed.map(|value| value.text.clone()))
            .or(summary)
            .unwrap_or_default(),
        parent_item_id: None,
    }
}

fn reasoning_snapshot_text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let parts: Vec<&str> = parts
                .iter()
                .filter_map(|part| part.as_str().filter(|text| !text.is_empty()))
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

fn longer(a: Option<String>, b: Option<&str>) -> Option<String> {
    match (a, b) {
        (None, None) => None,
        (Some(value), None) => Some(value),
        (None, Some(value)) => Some(value.to_owned()),
        (Some(left), Some(right)) => (right.len() > left.len())
            .then(|| right.to_owned())
            .or(Some(left)),
    }
}

const STATUS_MAP: &[(&str, ToolStatus)] = &[
    ("inProgress", ToolStatus::Running),
    ("in_progress", ToolStatus::Running),
    ("completed", ToolStatus::Completed),
    ("failed", ToolStatus::Failed),
    ("declined", ToolStatus::Declined),
];

fn tool_status(raw: &Map<String, Value>, event_type: ItemEventType) -> ToolStatus {
    STATUS_MAP
        .iter()
        .find_map(|(wire, status)| {
            (raw.get("status").and_then(Value::as_str) == Some(*wire)).then_some(*status)
        })
        .unwrap_or(if event_type.is_completed() {
            ToolStatus::Completed
        } else {
            ToolStatus::Running
        })
}

fn tool_item(
    raw: &Map<String, Value>,
    id: &str,
    kind: &str,
    event_type: ItemEventType,
    state: &CodexUiMapperState,
) -> UiToolItem {
    let status = tool_status(raw, event_type);
    let mut item = match kind {
        "commandExecution" => {
            let display = tool_display("commandExecution", Some(&Value::Object(raw.clone())));
            let mut item = UiToolItem {
                id: id.to_owned(),
                name: kind.to_owned(),
                tool_kind: display.tool_kind,
                title: display.title,
                status,
                input: raw
                    .get("command")
                    .cloned()
                    .map(|command| json!({"command": command})),
                output: None,
                error: None,
                diffs: None,
                locations: None,
                exit_code: as_finite_number(raw.get("exitCode")),
                parent_item_id: None,
            };
            item.output = as_nonempty_str(raw.get("aggregatedOutput"))
                .or_else(|| as_nonempty_str(raw.get("output")))
                .map(ToOwned::to_owned)
                .or_else(|| {
                    event_type
                        .is_completed()
                        .then(|| state.outputs.get(id).cloned())
                        .flatten()
                });
            item
        }
        "fileChange" => {
            let display = tool_display("fileChange", Some(&Value::Object(raw.clone())));
            let artifacts = change_artifacts(raw.get("changes"));
            UiToolItem {
                id: id.to_owned(),
                name: kind.to_owned(),
                tool_kind: display.tool_kind,
                title: display.title,
                status,
                input: None,
                output: None,
                error: None,
                diffs: artifacts.as_ref().map(|value| value.0.clone()),
                locations: artifacts.map(|value| value.1),
                exit_code: None,
                parent_item_id: None,
            }
        }
        "mcpToolCall" => {
            let display = tool_display("mcpToolCall", Some(&Value::Object(raw.clone())));
            let server = as_nonempty_str(raw.get("server"));
            let tool = as_nonempty_str(raw.get("tool"));
            UiToolItem {
                id: id.to_owned(),
                name: match (server, tool) {
                    (Some(server), Some(tool)) => format!("{server}.{tool}"),
                    _ => kind.to_owned(),
                },
                tool_kind: display.tool_kind,
                title: display.title,
                status,
                input: raw.get("arguments").cloned(),
                output: raw.get("result").map(|result| match result {
                    Value::String(text) => text.clone(),
                    value => json_string(value),
                }),
                error: None,
                diffs: None,
                locations: None,
                exit_code: None,
                parent_item_id: None,
            }
        }
        "webSearch" => {
            let display = tool_display("webSearch", Some(&Value::Object(raw.clone())));
            UiToolItem {
                id: id.to_owned(),
                name: kind.to_owned(),
                tool_kind: display.tool_kind,
                title: display.title,
                status,
                input: None,
                output: None,
                error: None,
                diffs: None,
                locations: None,
                exit_code: None,
                parent_item_id: None,
            }
        }
        _ => {
            let display = tool_display(kind, Some(&Value::Object(raw.clone())));
            UiToolItem {
                id: id.to_owned(),
                name: kind.to_owned(),
                tool_kind: display.tool_kind,
                title: display.title,
                status,
                input: Some(Value::Object(raw.clone())),
                output: None,
                error: None,
                diffs: None,
                locations: None,
                exit_code: None,
                parent_item_id: None,
            }
        }
    };
    if let Some(error) = error_message(raw.get("error")) {
        item.error = Some(error.to_owned());
    }
    item
}

fn change_artifacts(value: Option<&Value>) -> Option<(Vec<FileDiff>, Vec<ToolLocation>)> {
    let changes = value?.as_array()?;
    let mut diffs = Vec::new();
    let mut locations = Vec::new();
    for change in changes {
        let Some(change) = as_record(change) else {
            continue;
        };
        let Some(path) = as_nonempty_str(change.get("path")) else {
            continue;
        };
        diffs.push(FileDiff {
            path: path.to_owned(),
            old_text: None,
            new_text: None,
            unified: as_nonempty_str(change.get("diff")).map(ToOwned::to_owned),
        });
        locations.push(ToolLocation {
            path: path.to_owned(),
            line: None,
        });
    }
    (!diffs.is_empty()).then_some((diffs, locations))
}

fn plan_entries_of(raw: &Map<String, Value>) -> Option<Vec<PlanEntry>> {
    if let Some(list) = raw
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| raw.get("plan").and_then(Value::as_array))
    {
        return Some(
            list.iter()
                .filter_map(|entry| {
                    let entry = as_record(entry)?;
                    let content = as_nonempty_str(
                        entry
                            .get("text")
                            .or_else(|| entry.get("step"))
                            .or_else(|| entry.get("content")),
                    )?;
                    let status = if entry.get("completed").and_then(Value::as_bool).is_some() {
                        if entry.get("completed").and_then(Value::as_bool) == Some(true) {
                            PlanStatus::Completed
                        } else {
                            PlanStatus::Pending
                        }
                    } else {
                        match entry.get("status").and_then(Value::as_str) {
                            Some("completed") => PlanStatus::Completed,
                            Some("inProgress") | Some("in_progress") => PlanStatus::InProgress,
                            _ => PlanStatus::Pending,
                        }
                    };
                    Some(PlanEntry {
                        content: content.to_owned(),
                        status,
                        priority: None,
                        active_form: None,
                    })
                })
                .collect(),
        );
    }
    let text = as_nonempty_str(raw.get("text"))?;
    Some(vec![PlanEntry {
        content: text.to_owned(),
        status: PlanStatus::InProgress,
        priority: None,
        active_form: None,
    }])
}

fn review_item(id: &str, name: &str, status: ToolStatus) -> UiToolItem {
    UiToolItem {
        id: id.to_owned(),
        name: name.to_owned(),
        tool_kind: ToolKind::Task,
        title: "Review".to_owned(),
        status,
        input: None,
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    }
}

fn map_review_mode(
    raw: &Map<String, Value>,
    id: &str,
    kind: &str,
    event_type: ItemEventType,
    state: &CodexUiMapperState,
) -> CodexUiMapping {
    let status = tool_status(raw, event_type);
    if kind == "enteredReviewMode" {
        let mut events = Vec::new();
        if let Some(open_id) = &state.review_item_id
            && open_id != id
        {
            events.push(UiEvent::ItemCompleted {
                item: UiItem::Tool(review_item(
                    open_id,
                    "enteredReviewMode",
                    ToolStatus::Completed,
                )),
            });
        }
        events.push(event_type.event(UiItem::Tool(review_item(id, kind, status))));
        let mut next = state.clone();
        next.review_item_id = (!matches!(
            status,
            ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Declined
        ))
        .then(|| id.to_owned());
        return CodexUiMapping {
            events,
            state: next,
        };
    }
    let Some(open_id) = &state.review_item_id else {
        return CodexUiMapping {
            events: vec![UiEvent::ItemCompleted {
                item: UiItem::Tool(review_item(id, kind, ToolStatus::Completed)),
            }],
            state: state.clone(),
        };
    };
    let mut next = state.clone();
    next.review_item_id = None;
    CodexUiMapping {
        events: vec![UiEvent::ItemCompleted {
            item: UiItem::Tool(review_item(open_id, "enteredReviewMode", status)),
        }],
        state: next,
    }
}

fn map_sub_agent_activity(
    raw: &Map<String, Value>,
    id: &str,
    state: &CodexUiMapperState,
) -> CodexUiMapping {
    if as_nonempty_str(raw.get("kind")) != Some("started") || state.known_items.contains(id) {
        return no_events(state);
    }
    let agent_path = as_nonempty_str(raw.get("agentPath"));
    let agent_name = agent_path
        .and_then(|path| path.split('/').rfind(|part| !part.is_empty()))
        .map(|name| name.replace('_', " "));
    let display_input = agent_name.as_ref().map(|name| json!({"description": name}));
    let display = tool_display("Task", display_input.as_ref());
    let agent_thread_id = as_nonempty_str(raw.get("agentThreadId"));
    let mut input = Map::new();
    if let Some(path) = agent_path {
        input.insert("agentPath".to_owned(), Value::String(path.to_owned()));
    }
    if let Some(thread_id) = agent_thread_id {
        input.insert(
            "agentThreadId".to_owned(),
            Value::String(thread_id.to_owned()),
        );
    }
    let item = UiToolItem {
        id: id.to_owned(),
        name: "spawnAgent".to_owned(),
        tool_kind: ToolKind::Task,
        title: display.title,
        status: ToolStatus::Running,
        input: Some(Value::Object(input)),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    };
    let mut next = state.clone();
    next.known_items.insert(id.to_owned());
    if let Some(thread_id) = agent_thread_id {
        next.collab_tasks.insert(
            thread_id.to_owned(),
            CodexCollabTask {
                item_id: id.to_owned(),
                prompt: agent_name,
                model: None,
            },
        );
    }
    CodexUiMapping {
        events: vec![UiEvent::ItemStarted {
            item: UiItem::Tool(item),
        }],
        state: next,
    }
}

fn map_collab_tool_call(
    raw: &Map<String, Value>,
    id: &str,
    event_type: ItemEventType,
    state: &CodexUiMapperState,
) -> CodexUiMapping {
    let operation =
        as_nonempty_str(raw.get("tool")).or_else(|| as_nonempty_str(raw.get("operation")));
    let receiver_ids = string_array(
        raw.get("receiverThreadIds")
            .or_else(|| raw.get("receiver_thread_ids")),
    );
    let is_spawn = matches!(operation, Some("spawnAgent" | "spawn_agent"));
    if is_spawn {
        let prompt = as_nonempty_str(raw.get("prompt")).map(ToOwned::to_owned);
        let model = as_nonempty_str(raw.get("model")).map(ToOwned::to_owned);
        let task = CodexCollabTask {
            item_id: id.to_owned(),
            prompt,
            model,
        };
        let mut next = state.clone();
        for thread_id in receiver_ids {
            next.collab_tasks.insert(thread_id, task.clone());
        }
        let status = collab_status(
            raw,
            &string_array(
                raw.get("receiverThreadIds")
                    .or_else(|| raw.get("receiver_thread_ids")),
            ),
            event_type,
            state,
        );
        let mapped_event = if matches!(event_type, ItemEventType::Started) {
            ItemEventType::Started
        } else if matches!(status, ToolStatus::Running | ToolStatus::Pending) {
            ItemEventType::Updated
        } else {
            ItemEventType::Completed
        };
        return CodexUiMapping {
            events: vec![mapped_event.event(UiItem::Tool(collab_task_item(&task, status)))],
            state: next,
        };
    }
    let tasks: Vec<CodexCollabTask> = receiver_ids
        .iter()
        .filter_map(|thread_id| state.collab_tasks.get(thread_id).cloned())
        .collect();
    if tasks.is_empty() {
        return no_events(state);
    }
    let status = collab_status(raw, &receiver_ids, event_type, state);
    let mapped_event = if matches!(status, ToolStatus::Running | ToolStatus::Pending) {
        ItemEventType::Updated
    } else {
        ItemEventType::Completed
    };
    CodexUiMapping {
        events: tasks
            .iter()
            .map(|task| mapped_event.event(UiItem::Tool(collab_task_item(task, status))))
            .collect(),
        state: state.clone(),
    }
}

fn collab_task_item(task: &CodexCollabTask, status: ToolStatus) -> UiToolItem {
    let display_input = task
        .prompt
        .as_ref()
        .map(|prompt| json!({"description": prompt}));
    let display = tool_display("Task", display_input.as_ref());
    let mut input = Map::new();
    if let Some(prompt) = &task.prompt {
        input.insert("prompt".to_owned(), Value::String(prompt.clone()));
    }
    if let Some(model) = &task.model {
        input.insert("model".to_owned(), Value::String(model.clone()));
    }
    UiToolItem {
        id: task.item_id.clone(),
        name: "spawnAgent".to_owned(),
        tool_kind: ToolKind::Task,
        title: display.title,
        status,
        input: (!input.is_empty()).then_some(Value::Object(input)),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    }
}

fn collab_status(
    raw: &Map<String, Value>,
    receiver_ids: &[String],
    event_type: ItemEventType,
    _state: &CodexUiMapperState,
) -> ToolStatus {
    let states = raw
        .get("agentsStates")
        .and_then(as_record)
        .or_else(|| raw.get("agents_states").and_then(as_record));
    let statuses: Vec<&str> = receiver_ids
        .iter()
        .filter_map(|thread_id| states?.get(thread_id))
        .filter_map(as_record)
        .filter_map(|state| state.get("status").and_then(Value::as_str))
        .collect();
    if statuses
        .iter()
        .any(|status| matches!(*status, "errored" | "notFound" | "not_found"))
    {
        return ToolStatus::Failed;
    }
    if statuses.contains(&"interrupted") {
        return ToolStatus::Declined;
    }
    if !statuses.is_empty()
        && statuses
            .iter()
            .all(|status| matches!(*status, "completed" | "shutdown"))
    {
        return ToolStatus::Completed;
    }
    if statuses
        .iter()
        .any(|status| matches!(*status, "running" | "pendingInit" | "pending_init"))
    {
        return ToolStatus::Running;
    }
    tool_status(raw, event_type)
}

fn collab_parent_item_id(
    params: &Map<String, Value>,
    state: &CodexUiMapperState,
) -> Option<String> {
    let thread_id = thread_id_of(params)?;
    state
        .collab_tasks
        .get(thread_id)
        .map(|task| task.item_id.clone())
}

fn set_parent(item: &mut UiItem, parent_item_id: String) {
    match item {
        UiItem::Message(item) => item.parent_item_id = Some(parent_item_id),
        UiItem::Reasoning(item) => item.parent_item_id = Some(parent_item_id),
        UiItem::Tool(item) => item.parent_item_id = Some(parent_item_id),
    }
}

#[derive(Debug, Clone, Copy)]
enum DeltaField {
    Text,
    Reasoning,
    Output,
}

#[derive(Debug, Clone, Copy)]
enum ReasoningChannel {
    Text,
    Summary,
}

fn map_delta(
    params: &Map<String, Value>,
    state: &CodexUiMapperState,
    field: DeltaField,
    reasoning_channel: ReasoningChannel,
) -> CodexUiMapping {
    let Some(item_id) = as_nonempty_str(params.get("itemId")) else {
        return no_events(state);
    };
    let Some(delta) = params
        .get("delta")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return no_events(state);
    };
    let mut events = Vec::new();
    let mut next = state.clone();
    if !state.known_items.contains(item_id) {
        let mut item = synthesized_item(item_id, field);
        if let Some(parent_item_id) = collab_parent_item_id(params, state) {
            set_parent(&mut item, parent_item_id);
        }
        events.push(UiEvent::ItemStarted { item });
        next.known_items.insert(item_id.to_owned());
    }
    events.push(UiEvent::ItemDelta {
        item_id: item_id.to_owned(),
        field: match field {
            DeltaField::Text => coducktor_protocol::ItemDeltaField::Text,
            DeltaField::Reasoning => coducktor_protocol::ItemDeltaField::Reasoning,
            DeltaField::Output => coducktor_protocol::ItemDeltaField::Output,
        },
        delta: delta.to_owned(),
    });
    match field {
        DeltaField::Output => {
            let mut output = state.outputs.clone();
            output
                .entry(item_id.to_owned())
                .and_modify(|value| value.push_str(delta))
                .or_insert_with(|| delta.to_owned());
            next.outputs = output;
        }
        DeltaField::Reasoning => {
            let mut reasonings = state.reasonings.clone();
            let previous = reasonings.get(item_id).cloned().unwrap_or_default();
            let next_reasoning = match reasoning_channel {
                ReasoningChannel::Text => ReasoningAccumulator {
                    text: format!("{}{}", previous.text, delta),
                    summary: previous.summary,
                },
                ReasoningChannel::Summary => ReasoningAccumulator {
                    text: previous.text,
                    summary: format!("{}{}", previous.summary, delta),
                },
            };
            reasonings.insert(item_id.to_owned(), next_reasoning);
            next.reasonings = reasonings;
        }
        DeltaField::Text => {}
    }
    CodexUiMapping {
        events,
        state: next,
    }
}

fn synthesized_item(item_id: &str, field: DeltaField) -> UiItem {
    match field {
        DeltaField::Text => UiItem::Message(UiMessageItem {
            id: item_id.to_owned(),
            role: MessageRole::Assistant,
            text: String::new(),
            phase: None,
            parent_item_id: None,
        }),
        DeltaField::Reasoning => UiItem::Reasoning(UiReasoningItem {
            id: item_id.to_owned(),
            text: String::new(),
            parent_item_id: None,
        }),
        DeltaField::Output => UiItem::Tool(UiToolItem {
            id: item_id.to_owned(),
            name: "commandExecution".to_owned(),
            tool_kind: ToolKind::Execute,
            title: tool_display("commandExecution", None).title,
            status: ToolStatus::Running,
            input: None,
            output: None,
            error: None,
            diffs: None,
            locations: None,
            exit_code: None,
            parent_item_id: None,
        }),
    }
}

fn map_token_usage(params: &Map<String, Value>, state: &CodexUiMapperState) -> CodexUiMapping {
    let Some(token_usage) = params.get("tokenUsage").and_then(as_record) else {
        return no_events(state);
    };
    let Some(total) = token_usage.get("total").and_then(as_record) else {
        return no_events(state);
    };
    let Some(mut usage) = codex_usage(total) else {
        return no_events(state);
    };
    usage.context_window = as_finite_number(token_usage.get("modelContextWindow"));
    let last = token_usage
        .get("last")
        .and_then(as_record)
        .and_then(codex_usage);
    let mut next = state.clone();
    if state.current_turn_id.is_some()
        && let Some(last) = last
    {
        next.pending_turn_usage = Some(last);
    }
    CodexUiMapping {
        events: vec![UiEvent::UsageUpdated {
            usage,
            cost_usd: None,
        }],
        state: next,
    }
}

fn codex_usage(raw: &Map<String, Value>) -> Option<TokenUsage> {
    let input = nonnegative(raw.get("inputTokens"))?;
    let output = nonnegative(raw.get("outputTokens"))?;
    let cache_read = nonnegative(raw.get("cachedInputTokens"));
    let reasoning = nonnegative(raw.get("reasoningOutputTokens"));
    Some(TokenUsage {
        input,
        output,
        total: nonnegative(raw.get("totalTokens")).unwrap_or(input + output),
        cache_read,
        cache_write: None,
        reasoning,
        context_window: None,
    })
}

fn nonnegative(value: Option<&Value>) -> Option<f64> {
    let value = as_finite_number(value)?;
    (value >= 0.0).then_some(value)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| as_nonempty_str(Some(value)).map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn turn_id_of(params: &Map<String, Value>) -> Option<&str> {
    params
        .get("turn")
        .and_then(as_record)
        .and_then(|turn| as_nonempty_str(turn.get("id")))
        .or_else(|| as_nonempty_str(params.get("turnId")))
}

fn thread_id_of(params: &Map<String, Value>) -> Option<&str> {
    params
        .get("thread")
        .and_then(as_record)
        .and_then(|thread| as_nonempty_str(thread.get("id")))
        .or_else(|| as_nonempty_str(params.get("threadId")))
}

fn error_message(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(message)) if !message.is_empty() => Some(message),
        Some(value) => value
            .as_object()
            .and_then(|object| as_nonempty_str(object.get("message"))),
        None => None,
    }
}
