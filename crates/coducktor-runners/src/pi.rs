//! Pi RPC to protocol-v2 mapping.
//!
//! Pi's RPC stream separates the assistant message end, which carries usage,
//! from `agent_settled`, which closes the turn. The mapper keeps that usage in
//! explicit state until the latter arrives.

use std::collections::{BTreeMap, BTreeSet};

use coducktor_protocol::{
    FileDiff, PlanEntry, PlanStatus, StopReason, TokenUsage, ToolStatus, UiBackend, UiEvent,
    UiItem, UiMessageItem, UiReasoningItem, UiToolItem, tool_display,
};
use serde_json::{Map, Value};

use crate::wire::{as_finite_number, as_nonempty_str, as_record};

/// Explicit immutable state carried between Pi RPC messages.
#[derive(Debug, Clone, PartialEq)]
pub struct PiUiMapperState {
    pub session_started: bool,
    pub session_id: Option<String>,
    pub turn_seq: u64,
    pub turn_id: Option<String>,
    pub stop_reason: StopReason,
    pub turn_usage: Option<TokenUsage>,
    pub turn_cost_usd: Option<f64>,
    pub started_items: BTreeSet<String>,
    pub text_by_item: BTreeMap<String, String>,
    tools: BTreeMap<String, UiToolItem>,
}

/// The result of folding one message.
#[derive(Debug, Clone, PartialEq)]
pub struct PiUiMapping {
    pub events: Vec<UiEvent>,
    pub state: PiUiMapperState,
}

/// Creates an empty mapper state.
pub fn create_pi_ui_state() -> PiUiMapperState {
    PiUiMapperState {
        session_started: false,
        session_id: None,
        turn_seq: 0,
        turn_id: None,
        stop_reason: StopReason::EndTurn,
        turn_usage: None,
        turn_cost_usd: None,
        started_items: BTreeSet::new(),
        text_by_item: BTreeMap::new(),
        tools: BTreeMap::new(),
    }
}

/// Starts a Pi turn before the first RPC frame is read.
pub fn pi_turn_started(state: &PiUiMapperState) -> PiUiMapping {
    let turn_seq = state.turn_seq + 1;
    let turn_id = format!("turn_{turn_seq}");
    let mut next = state.clone();
    next.turn_seq = turn_seq;
    next.turn_id = Some(turn_id.clone());
    next.stop_reason = StopReason::EndTurn;
    PiUiMapping {
        events: vec![UiEvent::TurnStarted { turn_id }],
        state: next,
    }
}

/// Folds one parsed Pi RPC message. Unknown and malformed messages are no-ops.
pub fn map_pi_rpc_message(value: &Value, state: &PiUiMapperState) -> PiUiMapping {
    let Some(value) = as_record(value) else {
        return no_events(state);
    };
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return no_events(state);
    };
    if kind == "response" {
        return map_response(value, state);
    }
    match kind {
        "message_update" => map_message_update(value, state),
        "message_end" => map_message_end(value, state),
        "tool_execution_start" => map_tool_start(value, state),
        "tool_execution_update" => map_tool_update(value, state),
        "tool_execution_end" => map_tool_end(value, state),
        "agent_settled" => complete_turn(state),
        "extension_error" => {
            let message = as_nonempty_str(value.get("error"))
                .or_else(|| as_nonempty_str(value.get("message")))
                .unwrap_or("pi extension error")
                .to_owned();
            PiUiMapping {
                events: vec![UiEvent::SessionError {
                    message,
                    fatal: false,
                }],
                state: state.clone(),
            }
        }
        _ => no_events(state),
    }
}

fn no_events(state: &PiUiMapperState) -> PiUiMapping {
    PiUiMapping {
        events: Vec::new(),
        state: state.clone(),
    }
}

fn map_response(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    if value.get("command").and_then(Value::as_str) == Some("get_state")
        && value.get("success").and_then(Value::as_bool) == Some(true)
        && let Some(data) = value.get("data").and_then(as_record)
    {
        let Some(session_id) = as_nonempty_str(data.get("sessionId")) else {
            return no_events(state);
        };
        if state.session_started {
            return no_events(state);
        }
        let model = data
            .get("model")
            .and_then(as_record)
            .and_then(|model| as_nonempty_str(model.get("id")))
            .map(ToOwned::to_owned);
        let mut next = state.clone();
        next.session_started = true;
        next.session_id = Some(session_id.to_owned());
        return PiUiMapping {
            events: vec![UiEvent::SessionStarted {
                session_id: session_id.to_owned(),
                backend: UiBackend::Pi,
                model,
                cwd: None,
                tools: None,
            }],
            state: next,
        };
    }
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return PiUiMapping {
            events: vec![UiEvent::SessionError {
                message: rpc_error(value),
                fatal: false,
            }],
            state: state.clone(),
        };
    }
    no_events(state)
}

fn map_message_update(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    let Some(update) = value.get("assistantMessageEvent").and_then(as_record) else {
        return no_events(state);
    };
    let Some(update_type) = as_nonempty_str(update.get("type")) else {
        return no_events(state);
    };
    let content_index = as_finite_number(update.get("contentIndex")).unwrap_or(0.0);
    let Some(turn_id) = state.turn_id.as_ref() else {
        return no_events(state);
    };
    if update_type == "done" {
        let mut next = state.clone();
        next.stop_reason = if update.get("reason").and_then(Value::as_str) == Some("length") {
            StopReason::MaxTokens
        } else {
            StopReason::EndTurn
        };
        return PiUiMapping {
            events: Vec::new(),
            state: next,
        };
    }
    if update_type == "error" {
        let reason = if update.get("reason").and_then(Value::as_str) == Some("aborted") {
            StopReason::Cancelled
        } else {
            StopReason::Error
        };
        let message = update
            .get("error")
            .and_then(as_record)
            .and_then(|error| as_nonempty_str(error.get("errorMessage")))
            .unwrap_or(if reason == StopReason::Cancelled {
                "pi model cancelled"
            } else {
                "pi model error"
            })
            .to_owned();
        let mut next = state.clone();
        next.stop_reason = reason;
        return PiUiMapping {
            events: vec![UiEvent::SessionError {
                message,
                fatal: false,
            }],
            state: next,
        };
    }
    let field = if update_type.starts_with("thinking_") {
        Some(PiTextField::Reasoning)
    } else if update_type.starts_with("text_") {
        Some(PiTextField::Text)
    } else {
        None
    };
    let Some(field) = field else {
        return no_events(state);
    };
    let item_id = format!(
        "{turn_id}_{}_{}",
        match field {
            PiTextField::Text => "text",
            PiTextField::Reasoning => "reasoning",
        },
        format_index(content_index)
    );
    let make_item = |text: &str| -> UiItem {
        match field {
            PiTextField::Text => UiItem::Message(UiMessageItem {
                id: item_id.clone(),
                role: coducktor_protocol::MessageRole::Assistant,
                text: text.to_owned(),
                phase: None,
                parent_item_id: None,
            }),
            PiTextField::Reasoning => UiItem::Reasoning(UiReasoningItem {
                id: item_id.clone(),
                text: text.to_owned(),
                parent_item_id: None,
            }),
        }
    };
    let mut events = Vec::new();
    let mut next = state.clone();
    if !state.started_items.contains(&item_id) {
        events.push(UiEvent::ItemStarted {
            item: make_item(""),
        });
        next.started_items.insert(item_id.clone());
    }
    if let Some(delta) = update.get("delta").and_then(Value::as_str) {
        events.push(UiEvent::ItemDelta {
            item_id: item_id.clone(),
            field: match field {
                PiTextField::Text => coducktor_protocol::ItemDeltaField::Text,
                PiTextField::Reasoning => coducktor_protocol::ItemDeltaField::Reasoning,
            },
            delta: delta.to_owned(),
        });
        next.text_by_item
            .entry(item_id.clone())
            .and_modify(|text| text.push_str(delta))
            .or_insert_with(|| delta.to_owned());
    }
    if update_type.ends_with("_end") {
        let text = update
            .get("content")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| next.text_by_item.get(&item_id).cloned())
            .unwrap_or_default();
        events.push(UiEvent::ItemCompleted {
            item: make_item(&text),
        });
    }
    PiUiMapping {
        events,
        state: next,
    }
}

#[derive(Debug, Clone, Copy)]
enum PiTextField {
    Text,
    Reasoning,
}

fn format_index(index: f64) -> String {
    if index.fract() == 0.0 {
        format!("{index:.0}")
    } else {
        index.to_string()
    }
}

fn map_tool_start(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    let Some(id) = as_nonempty_str(value.get("toolCallId")) else {
        return no_events(state);
    };
    let Some(name) = as_nonempty_str(value.get("toolName")) else {
        return no_events(state);
    };
    let display = tool_display(name, value.get("args"));
    let mut item = UiToolItem {
        id: id.to_owned(),
        name: name.to_owned(),
        tool_kind: display.tool_kind,
        title: display.title,
        status: ToolStatus::Running,
        input: value.get("args").cloned(),
        output: None,
        error: None,
        diffs: None,
        locations: None,
        exit_code: None,
        parent_item_id: None,
    };
    item.diffs = tool_diffs(name, value.get("args"));
    let mut next = state.clone();
    next.tools.insert(id.to_owned(), item.clone());
    let mut events = vec![UiEvent::ItemStarted {
        item: UiItem::Tool(item),
    }];
    if let Some(entries) = tool_plan(name, value.get("args")) {
        events.push(UiEvent::PlanUpdated { entries });
    }
    PiUiMapping {
        events,
        state: next,
    }
}

fn map_tool_update(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    let Some(id) = as_nonempty_str(value.get("toolCallId")) else {
        return no_events(state);
    };
    let Some(previous) = state.tools.get(id) else {
        return no_events(state);
    };
    let Some(output) = content_text(
        value
            .get("partialResult")
            .and_then(as_record)
            .and_then(|result| result.get("content")),
    ) else {
        return no_events(state);
    };
    let mut item = previous.clone();
    item.output = Some(output);
    let mut next = state.clone();
    next.tools.insert(id.to_owned(), item.clone());
    PiUiMapping {
        events: vec![UiEvent::ItemUpdated {
            item: UiItem::Tool(item),
        }],
        state: next,
    }
}

fn map_tool_end(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    let Some(id) = as_nonempty_str(value.get("toolCallId")) else {
        return no_events(state);
    };
    let Some(previous) = state.tools.get(id) else {
        return no_events(state);
    };
    let result = value.get("result").and_then(as_record);
    let content = result.and_then(|result| result.get("content"));
    let output = content_text(content);
    let is_error = value.get("isError").and_then(Value::as_bool) == Some(true);
    let mut item = previous.clone();
    item.status = if is_error {
        ToolStatus::Failed
    } else {
        ToolStatus::Completed
    };
    if is_error {
        item.error = Some(output.unwrap_or_else(|| "pi tool failed".to_owned()));
    } else if let Some(output) = output {
        item.output = Some(output);
    }
    let mut next = state.clone();
    next.tools.insert(id.to_owned(), item.clone());
    let mut events = vec![UiEvent::ItemCompleted {
        item: UiItem::Tool(item),
    }];
    for (media_type, data) in tool_result_images(content) {
        events.push(UiEvent::Image {
            item_id: Some(id.to_owned()),
            media_type,
            data,
        });
    }
    PiUiMapping {
        events,
        state: next,
    }
}

fn complete_turn(state: &PiUiMapperState) -> PiUiMapping {
    let Some(turn_id) = state.turn_id.clone() else {
        return no_events(state);
    };
    let mut next = state.clone();
    next.turn_id = None;
    next.turn_usage = None;
    next.turn_cost_usd = None;
    PiUiMapping {
        events: vec![UiEvent::TurnCompleted {
            turn_id,
            stop_reason: state.stop_reason,
            usage: state.turn_usage.clone(),
            cost_usd: state.turn_cost_usd,
        }],
        state: next,
    }
}

fn map_message_end(value: &Map<String, Value>, state: &PiUiMapperState) -> PiUiMapping {
    let Some(message) = value.get("message").and_then(as_record) else {
        return no_events(state);
    };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return no_events(state);
    }
    let Some(usage_event) = usage_event(message.get("usage")) else {
        return no_events(state);
    };
    let mut next = state.clone();
    if let UiEvent::UsageUpdated { usage, cost_usd } = &usage_event {
        next.turn_usage = Some(usage.clone());
        next.turn_cost_usd = *cost_usd;
    }
    PiUiMapping {
        events: vec![usage_event],
        state: next,
    }
}

fn usage_event(value: Option<&Value>) -> Option<UiEvent> {
    let value = value.and_then(as_record)?;
    let input = as_finite_number(value.get("input")).unwrap_or(0.0);
    let output = as_finite_number(value.get("output")).unwrap_or(0.0);
    let cache_read = as_finite_number(value.get("cacheRead"));
    let cache_write = as_finite_number(value.get("cacheWrite"));
    let total = as_finite_number(value.get("totalTokens"))
        .unwrap_or(input + output + cache_read.unwrap_or(0.0) + cache_write.unwrap_or(0.0));
    if total <= 0.0 {
        return None;
    }
    let cost_usd = value
        .get("cost")
        .and_then(as_record)
        .and_then(|cost| as_finite_number(cost.get("total")));
    Some(UiEvent::UsageUpdated {
        usage: TokenUsage {
            input,
            output,
            total,
            cache_read,
            cache_write,
            reasoning: None,
            context_window: None,
        },
        cost_usd,
    })
}

fn tool_diffs(name: &str, input: Option<&Value>) -> Option<Vec<FileDiff>> {
    if !matches!(name.to_ascii_lowercase().as_str(), "edit" | "write") {
        return None;
    }
    let input = input.and_then(as_record)?;
    let path = as_nonempty_str(
        input
            .get("path")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("filePath")),
    )?;
    let old_text = any_string(input.get("oldText")).or_else(|| any_string(input.get("old_string")));
    let new_text = any_string(input.get("newText"))
        .or_else(|| any_string(input.get("new_string")))
        .or_else(|| any_string(input.get("content")));
    if old_text.is_none() && new_text.is_none() && !name.eq_ignore_ascii_case("write") {
        return None;
    }
    Some(vec![FileDiff {
        path: path.to_owned(),
        old_text,
        new_text,
        unified: None,
    }])
}

fn any_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(ToOwned::to_owned)
}

fn tool_plan(name: &str, input: Option<&Value>) -> Option<Vec<PlanEntry>> {
    if !matches!(
        name.to_ascii_lowercase().as_str(),
        "todowrite" | "todo_write"
    ) {
        return None;
    }
    let todos = input.and_then(as_record)?.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|todo| {
                let todo = as_record(todo)?;
                let content = as_nonempty_str(todo.get("content"))?;
                let status = match todo.get("status").and_then(Value::as_str) {
                    Some("pending") => PlanStatus::Pending,
                    Some("in_progress") => PlanStatus::InProgress,
                    Some("completed") => PlanStatus::Completed,
                    Some("cancelled") => PlanStatus::Cancelled,
                    _ => return None,
                };
                Some(PlanEntry {
                    content: content.to_owned(),
                    status,
                    priority: None,
                    active_form: None,
                })
            })
            .collect(),
    )
}

fn content_text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let parts: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    let part = as_record(part)?;
                    (part.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| part.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

/// Pi's image shape (`data`/`mimeType` directly on the part) — distinct from claude's
/// Anthropic-shaped `source.media_type`/`source.data`. Mirrors `pi_runner`'s
/// `tool_result_images`, which extracts the same shape for the durable event log; this is the
/// mapper-layer counterpart so the golden fixture contract actually covers what the runner does.
fn tool_result_images(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(parts)) = value else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            let record = as_record(part)?;
            if record.get("type").and_then(Value::as_str) != Some("image") {
                return None;
            }
            let data = as_nonempty_str(record.get("data"))?;
            let media_type = as_nonempty_str(record.get("mimeType"))?;
            Some((media_type.to_owned(), data.to_owned()))
        })
        .collect()
}

fn rpc_error(value: &Map<String, Value>) -> String {
    value
        .get("error")
        .and_then(as_record)
        .and_then(|error| as_nonempty_str(error.get("message")))
        .or_else(|| as_nonempty_str(value.get("message")))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "pi RPC command {} failed",
                as_nonempty_str(value.get("command")).unwrap_or("unknown")
            )
        })
}
