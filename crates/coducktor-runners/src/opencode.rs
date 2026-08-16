//! OpenCode SSE-bus to protocol-v2 mapping.
//!
//! The transport parses SSE framing before calling this module. This module
//! only folds the `{type, properties}` bus payload and owns the session/part
//! attribution rules.

use std::collections::{BTreeMap, BTreeSet};

use coducktor_protocol::{
    FileDiff, MessageRole, PlanEntry, PlanPriority, PlanStatus, StopReason, TokenUsage, ToolKind,
    ToolLocation, ToolStatus, UiBackend, UiEvent, UiItem, UiMessageItem, UiReasoningItem,
    UiToolItem, tool_display,
};
use serde_json::{Map, Value};

use crate::wire::{as_finite_number, as_nonempty_str, as_record};

#[derive(Debug, Clone, PartialEq)]
struct MessageUsage {
    info: Option<TokenUsage>,
    info_cost: Option<f64>,
    steps: Option<TokenUsage>,
    steps_cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct SubtaskScope {
    id: String,
    title: String,
    input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolSnapshot {
    status: ToolStatus,
    title: String,
}

/// Explicit immutable state carried between OpenCode bus events.
#[derive(Debug, Clone, PartialEq)]
pub struct OpencodeUiMapperState {
    pub session_id: Option<String>,
    pub session_started: bool,
    pub turn_seq: u64,
    pub current_turn_id: Option<String>,
    pub current_turn_message_ids: BTreeSet<String>,
    pub turn_errored: bool,
    pub msg_roles: BTreeMap<String, String>,
    pub cursors: BTreeMap<String, usize>,
    pub started_items: BTreeSet<String>,
    pub ended_items: BTreeSet<String>,
    tools: BTreeMap<String, ToolSnapshot>,
    pub last_plan_json: Option<String>,
    subtasks: BTreeMap<String, SubtaskScope>,
    unbound_subtasks: Vec<SubtaskScope>,
    usage_by_message: BTreeMap<String, MessageUsage>,
    pub last_usage: Option<(f64, Option<f64>)>,
}

/// The result of folding one event.
#[derive(Debug, Clone, PartialEq)]
pub struct OpencodeUiMapping {
    pub events: Vec<UiEvent>,
    pub state: OpencodeUiMapperState,
}

/// Creates an empty mapper state.
pub fn create_opencode_ui_state() -> OpencodeUiMapperState {
    OpencodeUiMapperState {
        session_id: None,
        session_started: false,
        turn_seq: 0,
        current_turn_id: None,
        current_turn_message_ids: BTreeSet::new(),
        turn_errored: false,
        msg_roles: BTreeMap::new(),
        cursors: BTreeMap::new(),
        started_items: BTreeSet::new(),
        ended_items: BTreeSet::new(),
        tools: BTreeMap::new(),
        last_plan_json: None,
        subtasks: BTreeMap::new(),
        unbound_subtasks: Vec::new(),
        usage_by_message: BTreeMap::new(),
        last_usage: None,
    }
}

/// Emits session.started for the response from POST /session.
pub fn opencode_session_started(
    session_id: &str,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    if state.session_started || session_id.is_empty() {
        return no_events(state);
    }
    let mut next = state.clone();
    next.session_id = Some(session_id.to_owned());
    next.session_started = true;
    OpencodeUiMapping {
        events: vec![UiEvent::SessionStarted {
            session_id: session_id.to_owned(),
            backend: UiBackend::OpenCode,
            model: None,
            cwd: None,
            tools: None,
        }],
        state: next,
    }
}

/// Starts the turn represented by the prompt POST.
pub fn opencode_turn_started(state: &OpencodeUiMapperState) -> OpencodeUiMapping {
    let turn_seq = state.turn_seq + 1;
    let turn_id = format!("turn_{turn_seq}");
    let mut next = state.clone();
    next.turn_seq = turn_seq;
    next.current_turn_id = Some(turn_id.clone());
    next.current_turn_message_ids.clear();
    next.turn_errored = false;
    OpencodeUiMapping {
        events: vec![UiEvent::TurnStarted { turn_id }],
        state: next,
    }
}

/// Folds one parsed OpenCode bus event. Unknown and malformed events are no-ops.
pub fn map_opencode_event(value: &Value, state: &OpencodeUiMapperState) -> OpencodeUiMapping {
    let Some(event) = as_record(value) else {
        return no_events(state);
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return no_events(state);
    };
    let properties = event.get("properties").and_then(as_record);
    let empty = Map::new();
    let properties = properties.unwrap_or(&empty);
    match event_type {
        "message.updated" | "message.created" | "message.completed" => {
            map_message_info(properties, state)
        }
        "message.part.updated" | "message.part.created" => map_part(properties, state),
        "session.idle" => map_idle(properties, state),
        "session.error" => map_session_error(properties, state),
        _ => no_events(state),
    }
}

fn no_events(state: &OpencodeUiMapperState) -> OpencodeUiMapping {
    OpencodeUiMapping {
        events: Vec::new(),
        state: state.clone(),
    }
}

fn map_message_info(
    properties: &Map<String, Value>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    let info = properties
        .get("info")
        .and_then(as_record)
        .unwrap_or(properties);
    let id = as_nonempty_str(info.get("id"));
    let role = as_nonempty_str(info.get("role"));
    let message_session = as_nonempty_str(info.get("sessionID"));
    let foreign = message_session.is_some()
        && state.session_id.is_some()
        && message_session != state.session_id.as_deref();
    let mut next = state.clone();
    if foreign && let Some(session_id) = message_session {
        next = resolve_subtask(session_id, &next).0;
    }
    if let (Some(id), Some(role)) = (id, role)
        && state.msg_roles.get(id).map(String::as_str) != Some(role)
    {
        next.msg_roles.insert(id.to_owned(), role.to_owned());
    }
    if let (Some(id), Some("assistant")) = (id, role)
        && next.current_turn_id.is_some()
        && !next.current_turn_message_ids.contains(id)
    {
        next.current_turn_message_ids.insert(id.to_owned());
    }
    if let (Some(id), Some("assistant")) = (id, role) {
        let usage = tokens_to_usage(info.get("tokens"));
        let cost = as_finite_number(info.get("cost"));
        if usage.is_some() || cost.is_some() {
            let previous = next
                .usage_by_message
                .get(id)
                .cloned()
                .unwrap_or_else(empty_message_usage);
            let merged = MessageUsage {
                info: match usage {
                    Some(usage)
                        if previous.info.is_none()
                            || usage.total
                                >= previous
                                    .info
                                    .as_ref()
                                    .map(|value| value.total)
                                    .unwrap_or(0.0) =>
                    {
                        Some(usage)
                    }
                    _ => previous.info,
                },
                info_cost: max_cost(previous.info_cost, cost),
                steps: previous.steps,
                steps_cost: previous.steps_cost,
            };
            next.usage_by_message.insert(id.to_owned(), merged);
            return emit_usage(next);
        }
    }
    OpencodeUiMapping {
        events: Vec::new(),
        state: next,
    }
}

fn map_part(properties: &Map<String, Value>, state: &OpencodeUiMapperState) -> OpencodeUiMapping {
    let part = properties
        .get("part")
        .and_then(as_record)
        .unwrap_or(properties);
    let Some(kind) = as_nonempty_str(part.get("type")) else {
        return no_events(state);
    };
    let message_id = as_nonempty_str(part.get("messageID"));
    let Some(id) = as_nonempty_str(part.get("id")).or(message_id) else {
        return no_events(state);
    };
    let part_session = as_nonempty_str(part.get("sessionID"));
    let foreign = part_session.is_some()
        && state.session_id.is_some()
        && part_session != state.session_id.as_deref();
    let mut next = state.clone();
    let parent_item_id = if foreign {
        let Some(session_id) = part_session else {
            return no_events(state);
        };
        let (resolved, subtask) = resolve_subtask(session_id, &next);
        let Some(subtask) = subtask else {
            return no_events(state);
        };
        next = resolved;
        Some(subtask.id)
    } else {
        None
    };
    if let Some(message_id) = message_id
        && next.msg_roles.get(message_id).map(String::as_str) != Some("assistant")
    {
        return OpencodeUiMapping {
            events: Vec::new(),
            state: next,
        };
    }
    match kind {
        "text" => map_text_like(part, properties, id, TextField::Text, parent_item_id, &next),
        "reasoning" => map_text_like(
            part,
            properties,
            id,
            TextField::Reasoning,
            parent_item_id,
            &next,
        ),
        "tool" => map_tool(part, id, parent_item_id, &next),
        "patch" => map_patch(part, id, parent_item_id, &next),
        "subtask" => map_subtask(part, id, &next),
        "step-finish" => map_step_finish(part, message_id, &next),
        _ => OpencodeUiMapping {
            events: Vec::new(),
            state: next,
        },
    }
}

#[derive(Debug, Clone, Copy)]
enum TextField {
    Text,
    Reasoning,
}

fn map_text_like(
    part: &Map<String, Value>,
    properties: &Map<String, Value>,
    id: &str,
    field: TextField,
    parent_item_id: Option<String>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    let full = part.get("text").and_then(Value::as_str).unwrap_or_default();
    let mut events = Vec::new();
    let mut next = state.clone();
    let make_item = |text: &str| -> UiItem {
        match field {
            TextField::Text => UiItem::Message(UiMessageItem {
                id: id.to_owned(),
                role: MessageRole::Assistant,
                text: text.to_owned(),
                phase: None,
                parent_item_id: parent_item_id.clone(),
            }),
            TextField::Reasoning => UiItem::Reasoning(UiReasoningItem {
                id: id.to_owned(),
                text: text.to_owned(),
                parent_item_id: parent_item_id.clone(),
            }),
        }
    };
    if !state.started_items.contains(id) {
        events.push(UiEvent::ItemStarted {
            item: make_item(""),
        });
        next.started_items.insert(id.to_owned());
    }

    let cursor = state.cursors.get(id).copied().unwrap_or(0);
    let server_delta = as_nonempty_str(properties.get("delta"));
    let (delta, next_cursor) = if let Some(delta) = server_delta {
        (
            Some(delta.to_owned()),
            (cursor + utf16_len(delta)).max(utf16_len(full)),
        )
    } else if utf16_len(full) > cursor {
        (utf16_slice_from(full, cursor), utf16_len(full))
    } else {
        (None, cursor)
    };
    if let Some(delta) = delta {
        events.push(UiEvent::ItemDelta {
            item_id: id.to_owned(),
            field: match field {
                TextField::Text => coducktor_protocol::ItemDeltaField::Text,
                TextField::Reasoning => coducktor_protocol::ItemDeltaField::Reasoning,
            },
            delta,
        });
        next.cursors.insert(id.to_owned(), next_cursor);
    }
    let finished = part
        .get("time")
        .and_then(as_record)
        .and_then(|time| as_finite_number(time.get("end")))
        .is_some();
    if finished && !state.ended_items.contains(id) {
        events.push(UiEvent::ItemCompleted {
            item: make_item(full),
        });
        next.ended_items.insert(id.to_owned());
    }
    OpencodeUiMapping {
        events,
        state: next,
    }
}

const STATUS_MAP: &[(&str, ToolStatus)] = &[
    ("pending", ToolStatus::Pending),
    ("running", ToolStatus::Running),
    ("completed", ToolStatus::Completed),
    ("error", ToolStatus::Failed),
];

fn map_tool(
    part: &Map<String, Value>,
    id: &str,
    parent_item_id: Option<String>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    let name = as_nonempty_str(part.get("tool"))
        .or_else(|| as_nonempty_str(part.get("name")))
        .unwrap_or("tool");
    let tool_state = part.get("state").and_then(as_record).unwrap_or(part);
    let previous = state.tools.get(id);
    let status = STATUS_MAP
        .iter()
        .find_map(|(wire, status)| {
            (tool_state.get("status").and_then(Value::as_str) == Some(*wire)).then_some(*status)
        })
        .or_else(|| previous.map(|value| value.status))
        .unwrap_or(ToolStatus::Pending);
    let input = tool_state.get("input");
    let display = tool_display(name, input);
    let title = as_nonempty_str(tool_state.get("title"))
        .or_else(|| previous.map(|value| value.title.as_str()))
        .unwrap_or(display.title.as_str())
        .to_owned();
    let mut item = UiToolItem {
        id: id.to_owned(),
        name: name.to_owned(),
        tool_kind: display.tool_kind,
        title: title.clone(),
        status,
        input: input.cloned(),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id,
    };
    if status == ToolStatus::Completed {
        item.output = as_nonempty_str(tool_state.get("output")).map(ToOwned::to_owned);
    }
    if status == ToolStatus::Failed {
        item.error = error_text(tool_state.get("error")).map(ToOwned::to_owned);
    }
    if let Some(metadata) = tool_state.get("metadata").and_then(as_record) {
        item.exit_code = as_finite_number(metadata.get("exit"));
    }
    let settled = matches!(status, ToolStatus::Completed | ToolStatus::Failed);
    let mut events = Vec::new();
    match previous {
        None => events.push(if settled {
            UiEvent::ItemCompleted {
                item: UiItem::Tool(item.clone()),
            }
        } else {
            UiEvent::ItemStarted {
                item: UiItem::Tool(item.clone()),
            }
        }),
        Some(previous) if previous.status != status => events.push(if settled {
            UiEvent::ItemCompleted {
                item: UiItem::Tool(item.clone()),
            }
        } else {
            UiEvent::ItemUpdated {
                item: UiItem::Tool(item.clone()),
            }
        }),
        Some(previous) if previous.title != title && !settled => {
            events.push(UiEvent::ItemUpdated {
                item: UiItem::Tool(item.clone()),
            })
        }
        Some(_) => {}
    }
    let mut next = state.clone();
    if !events.is_empty() {
        next.tools
            .insert(id.to_owned(), ToolSnapshot { status, title });
    }
    if name.eq_ignore_ascii_case("todowrite")
        && let Some(entries) = plan_entries_of(input)
    {
        let plan_json = serde_json::to_string(&entries).unwrap_or_default();
        if next.last_plan_json.as_deref() != Some(plan_json.as_str()) {
            next.last_plan_json = Some(plan_json);
            events.push(UiEvent::PlanUpdated { entries });
        }
    }
    OpencodeUiMapping {
        events,
        state: next,
    }
}

fn plan_entries_of(value: Option<&Value>) -> Option<Vec<PlanEntry>> {
    let todos = value?.as_object()?.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|todo| {
                let todo = as_record(todo)?;
                let content = todo.get("content").and_then(Value::as_str)?;
                let status = match todo.get("status").and_then(Value::as_str) {
                    Some("in_progress") => PlanStatus::InProgress,
                    Some("completed") => PlanStatus::Completed,
                    Some("cancelled") => PlanStatus::Cancelled,
                    _ => PlanStatus::Pending,
                };
                let priority = match todo.get("priority").and_then(Value::as_str) {
                    Some("high") => Some(PlanPriority::High),
                    Some("medium") => Some(PlanPriority::Medium),
                    Some("low") => Some(PlanPriority::Low),
                    _ => None,
                };
                Some(PlanEntry {
                    content: content.to_owned(),
                    status,
                    priority,
                    active_form: None,
                })
            })
            .collect(),
    )
}

fn map_patch(
    part: &Map<String, Value>,
    id: &str,
    parent_item_id: Option<String>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    if state.ended_items.contains(id) {
        return no_events(state);
    }
    let Some((diffs, locations)) = patch_artifacts(part.get("files")) else {
        return no_events(state);
    };
    let label = match locations.as_slice() {
        [location] => Some(location.path.clone()),
        locations if locations.len() > 1 => Some(format!("{} files", locations.len())),
        _ => None,
    };
    let item = UiToolItem {
        id: id.to_owned(),
        name: "patch".to_owned(),
        tool_kind: ToolKind::Edit,
        title: label
            .map(|value| format!("Edit {value}"))
            .unwrap_or_else(|| "Edit".to_owned()),
        status: ToolStatus::Completed,
        input: None,
        output: None,
        error: None,
        diffs: Some(diffs),
        locations: Some(locations),
        exit_code: None,
        parent_item_id,
    };
    let mut next = state.clone();
    next.ended_items.insert(id.to_owned());
    OpencodeUiMapping {
        events: vec![UiEvent::ItemCompleted {
            item: UiItem::Tool(item),
        }],
        state: next,
    }
}

fn patch_artifacts(value: Option<&Value>) -> Option<(Vec<FileDiff>, Vec<ToolLocation>)> {
    let mut diffs = Vec::new();
    let mut locations = Vec::new();
    match value {
        Some(Value::Array(files)) => {
            for file in files {
                if let Some(path) = file.as_str().filter(|path| !path.is_empty()) {
                    push_patch(&mut diffs, &mut locations, path, None);
                } else if let Some(file) = as_record(file)
                    && let Some(path) = as_nonempty_str(
                        file.get("path")
                            .or_else(|| file.get("file"))
                            .or_else(|| file.get("filename")),
                    )
                {
                    push_patch(
                        &mut diffs,
                        &mut locations,
                        path,
                        as_nonempty_str(file.get("diff"))
                            .or_else(|| as_nonempty_str(file.get("patch"))),
                    );
                }
            }
        }
        Some(Value::Object(files)) => {
            for (path, value) in files {
                if path.is_empty() {
                    continue;
                }
                let unified = value.as_str().map(ToOwned::to_owned).or_else(|| {
                    as_record(value).and_then(|file| {
                        as_nonempty_str(file.get("diff"))
                            .or_else(|| as_nonempty_str(file.get("patch")))
                            .map(ToOwned::to_owned)
                    })
                });
                push_patch(&mut diffs, &mut locations, path, unified.as_deref());
            }
        }
        _ => {}
    }
    (!diffs.is_empty()).then_some((diffs, locations))
}

fn push_patch(
    diffs: &mut Vec<FileDiff>,
    locations: &mut Vec<ToolLocation>,
    path: &str,
    unified: Option<&str>,
) {
    diffs.push(FileDiff {
        path: path.to_owned(),
        old_text: None,
        new_text: None,
        unified: unified.map(ToOwned::to_owned),
    });
    locations.push(ToolLocation {
        path: path.to_owned(),
        line: None,
    });
}

fn map_subtask(
    part: &Map<String, Value>,
    id: &str,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    if state.started_items.contains(id) {
        return no_events(state);
    }
    let description = as_nonempty_str(part.get("description"));
    let display_input = description.map(|description| {
        Value::Object(Map::from_iter([(
            "description".to_owned(),
            Value::String(description.to_owned()),
        )]))
    });
    let display = tool_display("task", display_input.as_ref());
    let mut input = Map::new();
    if let Some(prompt) = as_nonempty_str(part.get("prompt")) {
        input.insert("prompt".to_owned(), Value::String(prompt.to_owned()));
    }
    if let Some(description) = description {
        input.insert(
            "description".to_owned(),
            Value::String(description.to_owned()),
        );
    }
    if let Some(agent) = as_nonempty_str(part.get("agent")) {
        input.insert("agent".to_owned(), Value::String(agent.to_owned()));
    }
    let item = UiToolItem {
        id: id.to_owned(),
        name: "subtask".to_owned(),
        tool_kind: display.tool_kind,
        title: display.title,
        status: ToolStatus::Running,
        input: (!input.is_empty()).then_some(Value::Object(input)),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    };
    let scope = SubtaskScope {
        id: id.to_owned(),
        title: item.title.clone(),
        input: item.input.clone(),
    };
    let mut next = state.clone();
    next.started_items.insert(id.to_owned());
    next.unbound_subtasks.push(scope);
    OpencodeUiMapping {
        events: vec![UiEvent::ItemStarted {
            item: UiItem::Tool(item),
        }],
        state: next,
    }
}

fn map_step_finish(
    part: &Map<String, Value>,
    message_id: Option<&str>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    let Some(message_id) = message_id else {
        return no_events(state);
    };
    let tokens = tokens_to_usage(part.get("tokens"));
    let cost = as_finite_number(part.get("cost"));
    if tokens.is_none() && cost.is_none() {
        return no_events(state);
    }
    let previous = state
        .usage_by_message
        .get(message_id)
        .cloned()
        .unwrap_or_else(empty_message_usage);
    let merged = MessageUsage {
        info: previous.info,
        info_cost: previous.info_cost,
        steps: match tokens {
            Some(tokens) => Some(add_usage(previous.steps.as_ref(), &tokens)),
            None => previous.steps,
        },
        steps_cost: cost
            .map(|cost| previous.steps_cost.unwrap_or(0.0) + cost)
            .or(previous.steps_cost),
    };
    let mut next = state.clone();
    next.usage_by_message.insert(message_id.to_owned(), merged);
    if next.current_turn_id.is_some() {
        next.current_turn_message_ids.insert(message_id.to_owned());
    }
    emit_usage(next)
}

fn map_idle(properties: &Map<String, Value>, state: &OpencodeUiMapperState) -> OpencodeUiMapping {
    let session_id = as_nonempty_str(properties.get("sessionID"));
    let is_main = session_id.is_none()
        || state.session_id.is_none()
        || session_id == state.session_id.as_deref();
    if !is_main {
        let Some(session_id) = session_id else {
            return no_events(state);
        };
        let (resolved, subtask) = resolve_subtask(session_id, state);
        let Some(subtask) = subtask else {
            return no_events(state);
        };
        let mut next = resolved;
        next.subtasks.remove(session_id);
        return OpencodeUiMapping {
            events: vec![UiEvent::ItemCompleted {
                item: UiItem::Tool(completed_subtask(&subtask)),
            }],
            state: next,
        };
    }
    let Some(turn_id) = state.current_turn_id.clone() else {
        return no_events(state);
    };
    let mut events: Vec<UiEvent> = state
        .subtasks
        .values()
        .chain(state.unbound_subtasks.iter())
        .map(|subtask| UiEvent::ItemCompleted {
            item: UiItem::Tool(completed_subtask(subtask)),
        })
        .collect();
    let turn_usage = usage_for_messages(&state.current_turn_message_ids, &state.usage_by_message);
    events.push(UiEvent::TurnCompleted {
        turn_id,
        stop_reason: if state.turn_errored {
            StopReason::Error
        } else {
            StopReason::EndTurn
        },
        usage: turn_usage.0,
        cost_usd: turn_usage.1,
    });
    let mut next = state.clone();
    next.current_turn_id = None;
    next.current_turn_message_ids.clear();
    next.turn_errored = false;
    next.subtasks.clear();
    next.unbound_subtasks.clear();
    OpencodeUiMapping {
        events,
        state: next,
    }
}

fn map_session_error(
    properties: &Map<String, Value>,
    state: &OpencodeUiMapperState,
) -> OpencodeUiMapping {
    let session_id = as_nonempty_str(properties.get("sessionID"));
    let foreign = session_id.is_some()
        && state.session_id.is_some()
        && session_id != state.session_id.as_deref();
    let mut next = state.clone();
    if foreign {
        let Some(session_id) = session_id else {
            return no_events(state);
        };
        let (resolved, subtask) = resolve_subtask(session_id, &next);
        if subtask.is_none() {
            return no_events(state);
        }
        next = resolved;
    }
    let message = error_text(properties.get("error"))
        .unwrap_or("opencode session error")
        .to_owned();
    if next.current_turn_id.is_some() {
        next.turn_errored = true;
    }
    OpencodeUiMapping {
        events: vec![UiEvent::SessionError {
            message,
            fatal: false,
        }],
        state: next,
    }
}

fn resolve_subtask(
    session_id: &str,
    state: &OpencodeUiMapperState,
) -> (OpencodeUiMapperState, Option<SubtaskScope>) {
    if let Some(subtask) = state.subtasks.get(session_id) {
        return (state.clone(), Some(subtask.clone()));
    }
    if state.unbound_subtasks.len() != 1 {
        return (state.clone(), None);
    }
    let Some(subtask) = state.unbound_subtasks.first().cloned() else {
        return (state.clone(), None);
    };
    let mut next = state.clone();
    next.subtasks.insert(session_id.to_owned(), subtask.clone());
    next.unbound_subtasks.clear();
    (next, Some(subtask))
}

fn completed_subtask(scope: &SubtaskScope) -> UiToolItem {
    UiToolItem {
        id: scope.id.clone(),
        name: "subtask".to_owned(),
        tool_kind: ToolKind::Task,
        title: scope.title.clone(),
        status: ToolStatus::Completed,
        input: scope.input.clone(),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    }
}

fn empty_message_usage() -> MessageUsage {
    MessageUsage {
        info: None,
        info_cost: None,
        steps: None,
        steps_cost: None,
    }
}

fn tokens_to_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let tokens = value?.as_object()?;
    let input = as_finite_number(tokens.get("input")).unwrap_or(0.0);
    let output = as_finite_number(tokens.get("output")).unwrap_or(0.0);
    let reasoning = as_finite_number(tokens.get("reasoning"));
    let cache = tokens.get("cache").and_then(as_record);
    let cache_read = cache.and_then(|cache| as_finite_number(cache.get("read")));
    let cache_write = cache.and_then(|cache| as_finite_number(cache.get("write")));
    Some(TokenUsage {
        input,
        output,
        total: input
            + output
            + reasoning.unwrap_or(0.0)
            + cache_read.unwrap_or(0.0)
            + cache_write.unwrap_or(0.0),
        cache_read,
        cache_write,
        reasoning,
        context_window: None,
    })
}

fn add_usage(previous: Option<&TokenUsage>, current: &TokenUsage) -> TokenUsage {
    let Some(previous) = previous else {
        return current.clone();
    };
    TokenUsage {
        input: previous.input + current.input,
        output: previous.output + current.output,
        total: previous.total + current.total,
        cache_read: optional_sum(previous.cache_read, current.cache_read),
        cache_write: optional_sum(previous.cache_write, current.cache_write),
        reasoning: optional_sum(previous.reasoning, current.reasoning),
        context_window: None,
    }
}

fn optional_sum(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0.0) + right.unwrap_or(0.0)),
    }
}

fn max_cost(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (None, None) => None,
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (Some(left), Some(right)) => Some(left.max(right)),
    }
}

fn emit_usage(state: OpencodeUiMapperState) -> OpencodeUiMapping {
    let mut usage = TokenUsage {
        input: 0.0,
        output: 0.0,
        total: 0.0,
        cache_read: None,
        cache_write: None,
        reasoning: None,
        context_window: None,
    };
    let mut cost: Option<f64> = None;
    for message in state.usage_by_message.values() {
        let effective = if let Some(info) = &message.info {
            if message.steps.is_none()
                || info.total
                    >= message
                        .steps
                        .as_ref()
                        .map(|value| value.total)
                        .unwrap_or(0.0)
            {
                Some(info)
            } else {
                message.steps.as_ref()
            }
        } else {
            message.steps.as_ref()
        };
        if let Some(effective) = effective {
            usage = add_usage(Some(&usage), effective);
        }
        if let Some(effective_cost) = max_cost(message.info_cost, message.steps_cost) {
            cost = Some(cost.unwrap_or(0.0) + effective_cost);
        }
    }
    if usage.total == 0.0 && cost.unwrap_or(0.0) == 0.0 {
        return OpencodeUiMapping {
            events: Vec::new(),
            state,
        };
    }
    if state
        .last_usage
        .is_some_and(|last| last.0 == usage.total && last.1 == cost)
    {
        return OpencodeUiMapping {
            events: Vec::new(),
            state,
        };
    }
    let mut next = state;
    next.last_usage = Some((usage.total, cost));
    OpencodeUiMapping {
        events: vec![UiEvent::UsageUpdated {
            usage,
            cost_usd: cost,
        }],
        state: next,
    }
}

fn usage_for_messages(
    message_ids: &BTreeSet<String>,
    usage_by_message: &BTreeMap<String, MessageUsage>,
) -> (Option<TokenUsage>, Option<f64>) {
    let mut usage = None;
    let mut cost = None;
    for id in message_ids {
        let Some(message) = usage_by_message.get(id) else {
            continue;
        };
        let effective = if let Some(info) = &message.info {
            if message.steps.is_none()
                || info.total
                    >= message
                        .steps
                        .as_ref()
                        .map(|value| value.total)
                        .unwrap_or(0.0)
            {
                Some(info)
            } else {
                message.steps.as_ref()
            }
        } else {
            message.steps.as_ref()
        };
        if let Some(effective) = effective
            && (effective.input > 0.0 || effective.output > 0.0)
        {
            usage = Some(add_usage(usage.as_ref(), effective));
        }
        if let Some(effective_cost) = max_cost(message.info_cost, message.steps_cost)
            && effective_cost > 0.0
        {
            cost = Some(cost.unwrap_or(0.0) + effective_cost);
        }
    }
    (usage, cost)
}

fn error_text(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(message)) if !message.is_empty() => Some(message),
        Some(value) => {
            let value = as_record(value)?;
            as_nonempty_str(value.get("message"))
                .or_else(|| {
                    value
                        .get("data")
                        .and_then(as_record)
                        .and_then(|data| as_nonempty_str(data.get("message")))
                })
                .or_else(|| as_nonempty_str(value.get("name")))
        }
        None => None,
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn utf16_slice_from(value: &str, offset: usize) -> Option<String> {
    if offset == 0 {
        return Some(value.to_owned());
    }
    let mut units = 0;
    for (index, character) in value.char_indices() {
        if units >= offset {
            return Some(value[index..].to_owned());
        }
        units += character.len_utf16();
        if units >= offset {
            return (units == offset).then(|| value[index + character.len_utf8()..].to_owned());
        }
    }
    (units <= offset).then(String::new)
}
