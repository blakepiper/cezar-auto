//! Claude stream-json to protocol-v2 mapping.
//!
//! This is the Rust counterpart of `packages/cezar/src/core/claude-ui-mapper.ts`.
//! The mapper is pure: callers provide the previous state and thread the
//! returned state into the next frame. It emits no v1 events; the eventual
//! runner owns the parallel v1 stream.

use std::collections::BTreeMap;

use coducktor_protocol::{
    FileDiff, MessageRole, PlanEntry, PlanStatus, StopReason, TokenUsage, ToolLocation, ToolStatus,
    UiBackend, UiEvent, UiItem, UiMessageItem, UiReasoningItem, UiToolItem, tool_display,
};
use serde_json::{Map, Value};

use crate::wire::{as_finite_number, as_nonempty_str, as_record, json_string, value_string};

const TASK_CREATED_PREFIX: &str = "Task #";

/// Explicit immutable state carried between Claude frames.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeUiMapperState {
    pub fallback_session_id: Option<String>,
    pub session_started: bool,
    pub pending_turn_ids: Vec<String>,
    pub turn_seq: u64,
    pub current_turn_id: Option<String>,
    pub item_seq: u64,
    pub saw_assistant_text: bool,
    pub open_tools: BTreeMap<String, UiToolItem>,
    pub tasks: BTreeMap<String, PlanEntry>,
    pub pending_task_creates: BTreeMap<String, PlanEntry>,
}

/// The result of folding one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeUiMapping {
    pub events: Vec<UiEvent>,
    pub state: ClaudeUiMapperState,
}

/// Creates an empty mapper state.
pub fn create_claude_ui_state(fallback_session_id: Option<String>) -> ClaudeUiMapperState {
    ClaudeUiMapperState {
        fallback_session_id,
        session_started: false,
        pending_turn_ids: Vec::new(),
        turn_seq: 0,
        current_turn_id: None,
        item_seq: 0,
        saw_assistant_text: false,
        open_tools: BTreeMap::new(),
        tasks: BTreeMap::new(),
        pending_task_creates: BTreeMap::new(),
    }
}

/// Starts the turn represented by the user message written to Claude's stdin.
/// The first turn is queued until `system/init` so session.started remains first.
pub fn claude_turn_started(state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let turn_seq = state.turn_seq + 1;
    let turn_id = format!("turn_{turn_seq}");
    let mut next = state.clone();
    next.turn_seq = turn_seq;
    next.current_turn_id = Some(turn_id.clone());
    if !state.session_started {
        next.pending_turn_ids.push(turn_id);
        return ClaudeUiMapping {
            events: Vec::new(),
            state: next,
        };
    }
    ClaudeUiMapping {
        events: vec![UiEvent::TurnStarted { turn_id }],
        state: next,
    }
}

/// Folds one parsed Claude stream-json frame. Malformed and unknown frames are no-ops.
pub fn map_claude_message(value: &Value, state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let Some(message) = as_record(value) else {
        return no_events(state);
    };
    match message.get("type").and_then(Value::as_str) {
        Some("system") if message.get("subtype").and_then(Value::as_str) == Some("init") => {
            map_init(message, state)
        }
        Some("assistant") => map_assistant(message, state),
        Some("user") => map_tool_results(message, state),
        Some("result") => map_result(message, state),
        _ => no_events(state),
    }
}

fn no_events(state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    ClaudeUiMapping {
        events: Vec::new(),
        state: state.clone(),
    }
}

fn map_init(message: &Map<String, Value>, state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let session_id = as_nonempty_str(message.get("session_id"))
        .map(ToOwned::to_owned)
        .or_else(|| state.fallback_session_id.clone())
        .unwrap_or_default();
    let mut event = UiEvent::SessionStarted {
        session_id,
        backend: UiBackend::Claude,
        model: as_nonempty_str(message.get("model")).map(ToOwned::to_owned),
        cwd: as_nonempty_str(message.get("cwd")).map(ToOwned::to_owned),
        tools: None,
    };
    if let UiEvent::SessionStarted { tools, .. } = &mut event
        && let Some(raw_tools) = message.get("tools").and_then(Value::as_array)
    {
        *tools = Some(
            raw_tools
                .iter()
                .filter_map(|tool| tool.as_str().map(ToOwned::to_owned))
                .collect(),
        );
    }

    let mut events = vec![event];
    events.extend(
        state
            .pending_turn_ids
            .iter()
            .cloned()
            .map(|turn_id| UiEvent::TurnStarted { turn_id }),
    );
    let mut next = state.clone();
    next.session_started = true;
    next.pending_turn_ids.clear();
    ClaudeUiMapping {
        events,
        state: next,
    }
}

fn map_assistant(message: &Map<String, Value>, state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let content = message_content(message);
    let parent_item_id = as_nonempty_str(message.get("parent_tool_use_id")).map(ToOwned::to_owned);
    let mut events = Vec::new();
    let mut next = state.clone();

    for raw in content {
        let Some(block) = as_record(raw) else {
            continue;
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let Some(text) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                next.saw_assistant_text = true;
                let mut item = UiMessageItem {
                    id: next_item_id(&mut next.item_seq),
                    role: MessageRole::Assistant,
                    text: text.to_owned(),
                    phase: None,
                    parent_item_id: parent_item_id.clone(),
                };
                events.push(UiEvent::ItemStarted {
                    item: UiItem::Message(item.clone()),
                });
                item.text = text.to_owned();
                events.push(UiEvent::ItemCompleted {
                    item: UiItem::Message(item),
                });
            }
            Some("thinking") => {
                let Some(text) = block.get("thinking").and_then(Value::as_str) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                let item = UiReasoningItem {
                    id: next_item_id(&mut next.item_seq),
                    text: text.to_owned(),
                    parent_item_id: parent_item_id.clone(),
                };
                events.push(UiEvent::ItemStarted {
                    item: UiItem::Reasoning(item.clone()),
                });
                events.push(UiEvent::ItemCompleted {
                    item: UiItem::Reasoning(item),
                });
            }
            Some("tool_use") => {
                let Some(id) = block.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(name) = block.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let display = tool_display(name, block.get("input"));
                let mut item = UiToolItem {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    tool_kind: display.tool_kind,
                    title: display.title,
                    status: ToolStatus::Running,
                    input: block.get("input").cloned(),
                    output: None,
                    error: None,
                    diffs: None,
                    locations: None,
                    exit_code: None,
                    parent_item_id: parent_item_id.clone(),
                };
                if let Some(artifacts) = edit_artifacts(name, block.get("input")) {
                    item.diffs = artifacts.diffs;
                    item.locations = Some(artifacts.locations);
                }
                events.push(UiEvent::ItemStarted {
                    item: UiItem::Tool(item.clone()),
                });

                if name == "TodoWrite" {
                    if let Some(entries) = plan_entries(block.get("input")) {
                        events.push(UiEvent::PlanUpdated { entries });
                    }
                } else if parent_item_id.is_none() {
                    if let Some(entry) = pending_task_create(name, block.get("input")) {
                        next.pending_task_creates.insert(id.to_owned(), entry);
                    } else if let Some(updated) =
                        apply_task_update(name, block.get("input"), &next.tasks)
                    {
                        next.tasks = updated;
                        events.push(UiEvent::PlanUpdated {
                            entries: next.tasks.values().cloned().collect(),
                        });
                    }
                }
                next.open_tools.insert(id.to_owned(), item);
            }
            _ => {}
        }
    }

    ClaudeUiMapping {
        events,
        state: next,
    }
}

fn map_tool_results(message: &Map<String, Value>, state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let parent_item_id = as_nonempty_str(message.get("parent_tool_use_id")).map(ToOwned::to_owned);
    let mut events = Vec::new();
    let mut next = state.clone();
    for raw in message_content(message) {
        let Some(block) = as_record(raw) else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let open = next.open_tools.get(tool_use_id).cloned();
        let mut item = open.clone().unwrap_or_else(|| UiToolItem {
            id: tool_use_id.to_owned(),
            name: "unknown".to_owned(),
            tool_kind: coducktor_protocol::ToolKind::Other,
            title: "Tool".to_owned(),
            status: ToolStatus::Running,
            input: None,
            output: None,
            error: None,
            diffs: None,
            locations: None,
            exit_code: None,
            parent_item_id: parent_item_id.clone(),
        });
        let text = stringify_tool_result_content(block.get("content"));
        if block.get("is_error").and_then(Value::as_bool) == Some(true) {
            item.status = ToolStatus::Failed;
            item.error = Some(text.clone());
        } else {
            item.status = ToolStatus::Completed;
            item.output = Some(text.clone());
        }
        events.push(UiEvent::ItemCompleted {
            item: UiItem::Tool(item),
        });
        for image in tool_result_image_blocks(block.get("content")) {
            events.push(UiEvent::Image {
                item_id: Some(tool_use_id.to_owned()),
                media_type: image.media_type,
                data: image.data,
            });
        }

        if let Some(parked) = next.pending_task_creates.remove(tool_use_id) {
            if block.get("is_error").and_then(Value::as_bool) != Some(true)
                && let Some((id, entry)) = parse_task_created(&text).map(|id| (id, parked))
            {
                next.tasks.insert(id, entry);
                events.push(UiEvent::PlanUpdated {
                    entries: next.tasks.values().cloned().collect(),
                });
            }
        } else if let Some(open_item) = open.as_ref()
            && open_item.name.eq_ignore_ascii_case("TaskList")
            && open_item.parent_item_id.is_none()
            && block.get("is_error").and_then(Value::as_bool) != Some(true)
            && let Some(tasks) = apply_task_list(&text, &next.tasks)
            && !same_plan(&tasks, &next.tasks)
        {
            next.tasks = tasks;
            events.push(UiEvent::PlanUpdated {
                entries: next.tasks.values().cloned().collect(),
            });
        }
        next.open_tools.remove(tool_use_id);
    }
    ClaudeUiMapping {
        events,
        state: next,
    }
}

fn map_result(message: &Map<String, Value>, state: &ClaudeUiMapperState) -> ClaudeUiMapping {
    let mut events = Vec::new();
    let mut next = state.clone();
    if let Some(denials) = message.get("permission_denials").and_then(Value::as_array) {
        for denial in denials {
            let Some(raw) = as_record(denial) else {
                continue;
            };
            let Some(tool_name) = raw.get("tool_name").and_then(Value::as_str) else {
                continue;
            };
            let id = raw
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| next_item_id(&mut next.item_seq));
            let item = if let Some(open) = next.open_tools.remove(&id) {
                UiToolItem {
                    status: ToolStatus::Declined,
                    ..open
                }
            } else {
                let display = tool_display(tool_name, raw.get("tool_input"));
                UiToolItem {
                    id,
                    name: tool_name.to_owned(),
                    tool_kind: display.tool_kind,
                    title: display.title,
                    status: ToolStatus::Declined,
                    input: raw.get("tool_input").cloned(),
                    output: None,
                    error: None,
                    diffs: None,
                    locations: None,
                    exit_code: None,
                    parent_item_id: None,
                }
            };
            events.push(UiEvent::ItemCompleted {
                item: UiItem::Tool(item),
            });
        }
    }

    if !next.saw_assistant_text
        && let Some(result) = message.get("result").and_then(Value::as_str)
        && !result.is_empty()
    {
        next.saw_assistant_text = true;
        let item = UiMessageItem {
            id: next_item_id(&mut next.item_seq),
            role: MessageRole::Assistant,
            text: result.to_owned(),
            phase: None,
            parent_item_id: None,
        };
        events.push(UiEvent::ItemStarted {
            item: UiItem::Message(item.clone()),
        });
        events.push(UiEvent::ItemCompleted {
            item: UiItem::Message(item),
        });
    }

    let turn_id = next.current_turn_id.clone().unwrap_or_else(|| {
        next.turn_seq += 1;
        format!("turn_{}", next.turn_seq)
    });
    let usage = raw_token_usage(message.get("usage"));
    let cost_usd = as_finite_number(message.get("total_cost_usd"));
    events.push(UiEvent::TurnCompleted {
        turn_id,
        stop_reason: result_stop_reason(message),
        usage: usage.clone(),
        cost_usd,
    });
    if let Some(usage) = usage {
        events.push(UiEvent::UsageUpdated { usage, cost_usd });
    }
    next.current_turn_id = None;
    ClaudeUiMapping {
        events,
        state: next,
    }
}

fn next_item_id(sequence: &mut u64) -> String {
    *sequence += 1;
    format!("item_{sequence}")
}

fn message_content(message: &Map<String, Value>) -> &[Value] {
    message
        .get("message")
        .and_then(as_record)
        .and_then(|nested| nested.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

struct EditArtifacts {
    diffs: Option<Vec<FileDiff>>,
    locations: Vec<ToolLocation>,
}

fn edit_artifacts(name: &str, input: Option<&Value>) -> Option<EditArtifacts> {
    let key = name.to_ascii_lowercase();
    if key != "edit" && key != "write" {
        return None;
    }
    let input = input.and_then(as_record)?;
    let path = input.get("file_path").and_then(Value::as_str)?;
    if path.is_empty() {
        return None;
    }
    let locations = vec![ToolLocation {
        path: path.to_owned(),
        line: None,
    }];
    if key == "edit" {
        let diffs = match (
            input.get("old_string").and_then(Value::as_str),
            input.get("new_string").and_then(Value::as_str),
        ) {
            (Some(old_text), Some(new_text)) => Some(vec![FileDiff {
                path: path.to_owned(),
                old_text: Some(old_text.to_owned()),
                new_text: Some(new_text.to_owned()),
                unified: None,
            }]),
            _ => None,
        };
        return Some(EditArtifacts { diffs, locations });
    }
    Some(EditArtifacts {
        diffs: Some(vec![FileDiff {
            path: path.to_owned(),
            old_text: None,
            new_text: input
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            unified: None,
        }]),
        locations,
    })
}

fn pending_task_create(name: &str, input: Option<&Value>) -> Option<PlanEntry> {
    if !name.eq_ignore_ascii_case("TaskCreate") {
        return None;
    }
    let input = input.and_then(as_record)?;
    let content = input.get("subject").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    Some(PlanEntry {
        content: content.to_owned(),
        status: PlanStatus::Pending,
        priority: None,
        active_form: input
            .get("activeForm")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    })
}

fn apply_task_update(
    name: &str,
    input: Option<&Value>,
    tasks: &BTreeMap<String, PlanEntry>,
) -> Option<BTreeMap<String, PlanEntry>> {
    if !name.eq_ignore_ascii_case("TaskUpdate") {
        return None;
    }
    let input = input.and_then(as_record)?;
    let id = match input.get("taskId") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => return None,
    };
    let existing = tasks.get(&id)?;
    if input.get("status").and_then(Value::as_str) == Some("deleted") {
        let mut next = tasks.clone();
        next.remove(&id);
        return Some(next);
    }
    let mut entry = existing.clone();
    let mut changed = false;
    if let Some(status) = normalize_plan_status(input.get("status"))
        && status != entry.status
    {
        entry.status = status;
        changed = true;
    }
    if let Some(content) = input.get("subject").and_then(Value::as_str) {
        let content = content.trim();
        if !content.is_empty() && content != entry.content {
            entry.content = content.to_owned();
            changed = true;
        }
    }
    if let Some(active_form) = input
        .get("activeForm")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        && entry.active_form.as_deref() != Some(active_form)
    {
        entry.active_form = Some(active_form.to_owned());
        changed = true;
    }
    if !changed {
        return None;
    }
    let mut next = tasks.clone();
    next.insert(id, entry);
    Some(next)
}

fn normalize_plan_status(value: Option<&Value>) -> Option<PlanStatus> {
    match value.and_then(Value::as_str) {
        Some("pending") => Some(PlanStatus::Pending),
        Some("in_progress") | Some("running") => Some(PlanStatus::InProgress),
        Some("completed") => Some(PlanStatus::Completed),
        _ => None,
    }
}

fn plan_entries(input: Option<&Value>) -> Option<Vec<PlanEntry>> {
    let todos = input.and_then(as_record)?.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|todo| {
                let todo = as_record(todo)?;
                let content = todo.get("content").and_then(Value::as_str)?;
                let status = normalize_plan_status(todo.get("status"))?;
                Some(PlanEntry {
                    content: content.to_owned(),
                    status,
                    priority: None,
                    active_form: todo
                        .get("activeForm")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                })
            })
            .collect(),
    )
}

fn parse_task_created(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix(TASK_CREATED_PREFIX)?;
    let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let id = &rest[..digit_count];
    rest[digit_count..]
        .starts_with(" created successfully")
        .then(|| id.to_owned())
}

fn apply_task_list(
    text: &str,
    tasks: &BTreeMap<String, PlanEntry>,
) -> Option<BTreeMap<String, PlanEntry>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut next = BTreeMap::new();
    for line in lines {
        let close = line.find("] ")?;
        let prefix = &line[..close];
        let content = line[close + 2..].trim();
        let open = prefix.find(" [")?;
        let id = prefix.strip_prefix('#')?.get(..open - 1)?;
        if id.is_empty() || content.is_empty() {
            return None;
        }
        let status = normalize_plan_status(Some(&Value::String(prefix[open + 2..].to_owned())))?;
        next.insert(
            id.to_owned(),
            PlanEntry {
                content: content.to_owned(),
                status,
                priority: None,
                active_form: tasks.get(id).and_then(|entry| entry.active_form.clone()),
            },
        );
    }
    Some(next)
}

fn same_plan(left: &BTreeMap<String, PlanEntry>, right: &BTreeMap<String, PlanEntry>) -> bool {
    left == right
}

pub(crate) fn stringify_tool_result_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                let Some(record) = as_record(part) else {
                    return json_string(part);
                };
                match record.get("type").and_then(Value::as_str) {
                    Some("text") => record
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| json_string(part)),
                    Some("image") => "[screenshot]".to_owned(),
                    _ => json_string(part),
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => json_string(value),
        None => value_string(None),
    }
}

pub(crate) struct ImageBlock {
    pub(crate) media_type: String,
    pub(crate) data: String,
}

pub(crate) fn tool_result_image_blocks(content: Option<&Value>) -> Vec<ImageBlock> {
    content
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    let record = as_record(part)?;
                    if record.get("type").and_then(Value::as_str) != Some("image") {
                        return None;
                    }
                    let source = record.get("source").and_then(as_record)?;
                    if source.get("type").and_then(Value::as_str) != Some("base64") {
                        return None;
                    }
                    Some(ImageBlock {
                        media_type: source.get("media_type").and_then(Value::as_str)?.to_owned(),
                        data: source.get("data").and_then(Value::as_str)?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn result_stop_reason(message: &Map<String, Value>) -> StopReason {
    match message.get("subtype").and_then(Value::as_str) {
        Some("success") => StopReason::EndTurn,
        Some("error_max_turns") => StopReason::MaxTokens,
        Some("error_during_execution") => StopReason::Error,
        _ if message.get("is_error").and_then(Value::as_bool) == Some(true) => StopReason::Error,
        _ => StopReason::EndTurn,
    }
}

fn raw_token_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let usage = value.and_then(as_record)?;
    let input = as_finite_number(usage.get("input_tokens")).unwrap_or(0.0);
    let output = as_finite_number(usage.get("output_tokens")).unwrap_or(0.0);
    let cache_read = as_finite_number(usage.get("cache_read_input_tokens"));
    let cache_write = as_finite_number(usage.get("cache_creation_input_tokens"));
    Some(TokenUsage {
        input,
        output,
        total: input + output + cache_read.unwrap_or(0.0) + cache_write.unwrap_or(0.0),
        cache_read,
        cache_write,
        reasoning: None,
        context_window: None,
    })
}
