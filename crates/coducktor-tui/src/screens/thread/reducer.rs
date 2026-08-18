//! The thread reducer folds one run's ordered event list into renderable turns. It handles
//! protocol-v2 events (`item.*`,
//! `turn.*`, `plan.updated`, `ask.requested`) plus every v1 line that has no v2 counterpart
//! (`user-message`, `note`/`lifecycle`, `error`, `image`, `check-output`,
//! `provider-auth-required`).
//!
//! Legacy v1 events without v2 equivalents remain supported where they are part of the durable
//! event vocabulary; malformed events cost one event and never panic the reducer.
//!
//! Pure and total: called with the full event list, it must never panic on a malformed event —
//! one bad event costs one event, never the fold.

use std::collections::HashMap;

use coducktor_contract::RunEvent;
use coducktor_protocol::{PlanEntry, StopReason, UiAskOption, UiAskQuestion, UiItem};
use serde_json::Value;

/// A dim/warning/danger transcript line (v1 note/lifecycle/error, v2 non-fatal session.error).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadNote {
    pub id: String,
    pub text: String,
    pub tone: NoteTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteTone {
    Dim,
    Warning,
    Danger,
}

/// An image the run persisted (v1 `image` line).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadImage {
    pub id: String,
    pub url: String,
    pub name: Option<String>,
}

/// A structured AskUser question the agent posed via `DUCK:ASK` (v2 `ask.requested`).
/// Resolution is client-side: the next `user-message` for the run flips `resolved` and
/// records the reply as `answer`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadAsk {
    pub id: String,
    pub questions: Vec<UiAskQuestion>,
    pub resolved: bool,
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthProvider {
    Claude,
    Codex,
    OpenCode,
    Pi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadProviderAuthRequired {
    pub id: String,
    pub provider: AuthProvider,
    pub auth_failure_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThreadEntry {
    Item(UiItem),
    Note(ThreadNote),
    Image(ThreadImage),
    Ask(ThreadAsk),
    ProviderAuthRequired(ThreadProviderAuthRequired),
}

impl ThreadEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Item(UiItem::Message(item)) => &item.id,
            Self::Item(UiItem::Reasoning(item)) => &item.id,
            Self::Item(UiItem::Tool(item)) => &item.id,
            Self::Note(note) => &note.id,
            Self::Image(image) => &image.id,
            Self::Ask(ask) => &ask.id,
            Self::ProviderAuthRequired(card) => &card.id,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadUserMessage {
    pub text: String,
    pub image_count: u64,
    pub images: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadCompleted {
    pub stop_reason: StopReason,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTurn {
    pub id: String,
    pub turn_id: Option<String>,
    pub user_message: Option<ThreadUserMessage>,
    pub items: Vec<ThreadEntry>,
    pub plan_entries: Option<Vec<PlanEntry>>,
    pub completed: Option<ThreadCompleted>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionEnded {
    pub reason: StopReason,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadState {
    pub turns: Vec<ThreadTurn>,
    pub session_ended: Option<SessionEnded>,
}

/// What the strip under the thread says. Pure so the mapping is table-testable.
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadFooter {
    Waiting,
    Closed { label: String, danger: bool },
}

pub fn thread_footer(
    status: coducktor_contract::RunStatus,
    error: Option<&str>,
) -> Option<ThreadFooter> {
    use coducktor_contract::RunStatus;
    match status {
        RunStatus::Waiting => Some(ThreadFooter::Waiting),
        RunStatus::Failed => Some(ThreadFooter::Closed {
            label: match error {
                Some(error) => format!("Session failed — {error}"),
                None => "Session failed".to_owned(),
            },
            danger: true,
        }),
        RunStatus::Review => Some(ThreadFooter::Closed {
            label: "Session closed — waiting for your review".to_owned(),
            danger: false,
        }),
        RunStatus::Done | RunStatus::Cancelled => Some(ThreadFooter::Closed {
            label: "Session closed".to_owned(),
            danger: false,
        }),
        RunStatus::Queued | RunStatus::Running => None,
    }
}

/// The plan the dock shows: the LATEST snapshot across all turns (full-replacement
/// semantics). An empty latest snapshot is returned as-is.
pub fn latest_plan_entries(state: &ThreadState) -> Option<&[PlanEntry]> {
    state
        .turns
        .iter()
        .rev()
        .find_map(|turn| turn.plan_entries.as_deref())
}

/// Every file path the run's tool items are known to have touched, most recently touched
/// first — the composer's `@` mention fallback.
pub fn thread_file_paths(state: &ThreadState) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for turn in &state.turns {
        for entry in &turn.items {
            if let ThreadEntry::Item(UiItem::Tool(tool)) = entry {
                for location in tool.locations.iter().flatten() {
                    seen.push(location.path.clone());
                }
                for diff in tool.diffs.iter().flatten() {
                    seen.push(diff.path.clone());
                }
            }
        }
    }
    let mut deduped: Vec<String> = Vec::new();
    for path in seen.into_iter().rev() {
        if !deduped.contains(&path) {
            deduped.push(path);
        }
    }
    deduped
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadReduceOptions {
    /// The current assistant turn still belongs to a running session — a complete trailing
    /// ask marker is provisional protocol text until turn-end resolves it.
    pub active_turn: bool,
}

pub fn reduce_thread(events: &[RunEvent], options: ThreadReduceOptions) -> ThreadState {
    let mut turns: Vec<ThreadTurn> = Vec::new();
    let mut items_by_key: HashMap<String, (usize, usize)> = HashMap::new();
    let mut pending_ask: Option<(usize, usize)> = None;
    let mut session_ended: Option<SessionEnded> = None;
    let mut turn_seq: u64 = 0;

    let new_turn = |turns: &mut Vec<ThreadTurn>, turn_seq: &mut u64, source_seq: Option<f64>| {
        *turn_seq += 1;
        let id = match source_seq {
            Some(seq) => format!("turn-seq-{seq}"),
            None => format!("turn-fallback-{turn_seq}"),
        };
        turns.push(ThreadTurn {
            id,
            turn_id: None,
            user_message: None,
            items: Vec::new(),
            plan_entries: None,
            completed: None,
        });
        turns.len() - 1
    };

    let item_key = |step_id: &Option<String>, item_id: &str| match step_id {
        Some(step_id) => format!("{step_id}:{item_id}"),
        None => item_id.to_owned(),
    };

    for event in events {
        let extra = &event.extra;
        match event.event_type.as_str() {
            "user-message" => {
                let text = str_field(extra, "text").unwrap_or_default();
                if let Some((turn_idx, entry_idx)) = pending_ask.take()
                    && let ThreadEntry::Ask(ask) = &mut turns[turn_idx].items[entry_idx]
                    && !ask.resolved
                {
                    ask.resolved = true;
                    if !text.is_empty() {
                        ask.answer = Some(text.clone());
                    }
                }
                let turn_idx = new_turn(&mut turns, &mut turn_seq, Some(event.seq));
                turns[turn_idx].user_message = Some(ThreadUserMessage {
                    text,
                    image_count: extra.get("imageCount").and_then(Value::as_u64).unwrap_or(0),
                    images: extra
                        .get("images")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            "turn.started" => {
                let turn_id = str_field(extra, "turnId");
                if let Some(last) = turns.last_mut()
                    && last.turn_id.is_none()
                {
                    last.turn_id = turn_id;
                } else {
                    let idx = new_turn(&mut turns, &mut turn_seq, Some(event.seq));
                    turns[idx].turn_id = turn_id;
                }
            }
            "turn.completed" => {
                let turn_id = str_field(extra, "turnId");
                let matched = turns
                    .iter()
                    .rposition(|turn| turn.turn_id == turn_id && turn_id.is_some());
                let idx = matched.or_else(|| turns.len().checked_sub(1));
                if let Some(idx) = idx {
                    turns[idx].completed = Some(ThreadCompleted {
                        stop_reason: extra
                            .get("stopReason")
                            .and_then(|value| serde_json::from_value(value.clone()).ok())
                            .unwrap_or(StopReason::EndTurn),
                        cost_usd: extra.get("costUsd").and_then(Value::as_f64),
                    });
                }
            }
            "item.started" | "item.updated" | "item.completed" => {
                let Some(item_value) = extra.get("item") else {
                    continue;
                };
                let Ok(item) = serde_json::from_value::<UiItem>(item_value.clone()) else {
                    continue;
                };
                let id = item_id(&item).to_owned();
                if id.is_empty() {
                    continue;
                }
                let key = item_key(&event.step_id, &id);
                if let Some(&(turn_idx, entry_idx)) = items_by_key.get(&key) {
                    turns[turn_idx].items[entry_idx] = ThreadEntry::Item(item);
                } else {
                    let turn_idx = current_turn(&mut turns, &mut turn_seq);
                    turns[turn_idx].items.push(ThreadEntry::Item(item));
                    let entry_idx = turns[turn_idx].items.len() - 1;
                    items_by_key.insert(key, (turn_idx, entry_idx));
                }
            }
            "item.delta" => {
                let item_id_value = str_field(extra, "itemId").unwrap_or_default();
                let key = item_key(&event.step_id, &item_id_value);
                let Some(&(turn_idx, entry_idx)) = items_by_key.get(&key) else {
                    continue;
                };
                let delta = str_field(extra, "delta").unwrap_or_default();
                if delta.is_empty() {
                    continue;
                }
                let field = str_field(extra, "field").unwrap_or_default();
                if let ThreadEntry::Item(item) = &mut turns[turn_idx].items[entry_idx] {
                    match item {
                        UiItem::Tool(tool) if field == "output" => {
                            let output = tool.output.get_or_insert_with(String::new);
                            output.push_str(&delta);
                        }
                        UiItem::Message(message) if field != "output" => {
                            message.text.push_str(&delta)
                        }
                        UiItem::Reasoning(reasoning) if field != "output" => {
                            reasoning.text.push_str(&delta)
                        }
                        _ => {}
                    }
                }
            }
            // The runner seam still stores normalized v1 events for Claude, Codex, OpenCode, and
            // pi. Keep those events visible until every backend emits the v2 item union directly;
            // dropping them makes a live run look frozen even though the durable stream is moving.
            "text" => {
                let text = str_field(extra, "text").unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let turn_idx = current_turn(&mut turns, &mut turn_seq);
                let key = format!("legacy-message:{turn_idx}");
                if let Some(&(item_turn, entry_idx)) = items_by_key.get(&key) {
                    if let ThreadEntry::Item(UiItem::Message(message)) =
                        &mut turns[item_turn].items[entry_idx]
                    {
                        message.text.push_str(&text);
                    }
                } else {
                    let item = UiItem::Message(coducktor_protocol::UiMessageItem {
                        id: format!("v1-message:{turn_idx}"),
                        role: coducktor_protocol::MessageRole::Assistant,
                        text,
                        phase: None,
                        parent_item_id: None,
                    });
                    turns[turn_idx].items.push(ThreadEntry::Item(item));
                    items_by_key.insert(key, (turn_idx, turns[turn_idx].items.len() - 1));
                }
            }
            "reasoning" => {
                let text = str_field(extra, "text").unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let turn_idx = current_turn(&mut turns, &mut turn_seq);
                let key = format!("legacy-reasoning:{turn_idx}");
                if let Some(&(item_turn, entry_idx)) = items_by_key.get(&key) {
                    if let ThreadEntry::Item(UiItem::Reasoning(reasoning)) =
                        &mut turns[item_turn].items[entry_idx]
                    {
                        reasoning.text.push_str(&text);
                    }
                } else {
                    let item = UiItem::Reasoning(coducktor_protocol::UiReasoningItem {
                        id: format!("v1-reasoning:{turn_idx}"),
                        text,
                        parent_item_id: None,
                    });
                    turns[turn_idx].items.push(ThreadEntry::Item(item));
                    items_by_key.insert(key, (turn_idx, turns[turn_idx].items.len() - 1));
                }
            }
            "tool-call" => {
                let id = str_field(extra, "id")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| format!("v1-tool:{}", event.seq));
                let name = str_field(extra, "tool")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "tool".to_owned());
                let input = extra.get("input").cloned();
                let display = coducktor_protocol::tool_display(&name, input.as_ref());
                let item = UiItem::Tool(coducktor_protocol::UiToolItem {
                    id: id.clone(),
                    name,
                    tool_kind: display.tool_kind,
                    title: display.title,
                    status: coducktor_protocol::ToolStatus::Running,
                    input,
                    output: None,
                    error: None,
                    diffs: None,
                    locations: None,
                    exit_code: None,
                    parent_item_id: None,
                });
                let key = item_key(&event.step_id, &id);
                let turn_idx = current_turn(&mut turns, &mut turn_seq);
                if let Some(&(item_turn, entry_idx)) = items_by_key.get(&key) {
                    turns[item_turn].items[entry_idx] = ThreadEntry::Item(item);
                } else {
                    turns[turn_idx].items.push(ThreadEntry::Item(item));
                    items_by_key.insert(key, (turn_idx, turns[turn_idx].items.len() - 1));
                }
            }
            "tool-result" => {
                let id = str_field(extra, "toolCallId")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| format!("v1-tool-result:{}", event.seq));
                let result = value_text(extra.get("result"));
                let failed = extra
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let key = item_key(&event.step_id, &id);
                if let Some(&(item_turn, entry_idx)) = items_by_key.get(&key) {
                    if let ThreadEntry::Item(UiItem::Tool(tool)) =
                        &mut turns[item_turn].items[entry_idx]
                    {
                        tool.status = if failed {
                            coducktor_protocol::ToolStatus::Failed
                        } else {
                            coducktor_protocol::ToolStatus::Completed
                        };
                        if failed {
                            tool.error = Some(result);
                        } else {
                            tool.output = Some(result);
                        }
                    }
                } else {
                    let turn_idx = current_turn(&mut turns, &mut turn_seq);
                    let display = coducktor_protocol::tool_display("tool", None);
                    let item = UiItem::Tool(coducktor_protocol::UiToolItem {
                        id: id.clone(),
                        name: "tool".to_owned(),
                        tool_kind: display.tool_kind,
                        title: display.title,
                        status: if failed {
                            coducktor_protocol::ToolStatus::Failed
                        } else {
                            coducktor_protocol::ToolStatus::Completed
                        },
                        input: None,
                        output: (!failed).then_some(result.clone()),
                        error: failed.then_some(result),
                        diffs: None,
                        locations: None,
                        exit_code: extra.get("exitCode").and_then(Value::as_f64),
                        parent_item_id: None,
                    });
                    turns[turn_idx].items.push(ThreadEntry::Item(item));
                    items_by_key.insert(key, (turn_idx, turns[turn_idx].items.len() - 1));
                }
            }
            "plan.updated" => {
                let Some(entries) = extra
                    .get("entries")
                    .and_then(|value| serde_json::from_value::<Vec<PlanEntry>>(value.clone()).ok())
                else {
                    continue;
                };
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].plan_entries = Some(entries);
            }
            "ask.requested" => {
                let Some(request_id) = str_field(extra, "requestId") else {
                    continue;
                };
                let Some(raw_questions) = extra.get("questions").and_then(Value::as_array) else {
                    continue;
                };
                let questions: Vec<UiAskQuestion> = raw_questions
                    .iter()
                    .filter_map(valid_ask_question)
                    .collect();
                if questions.is_empty() {
                    continue;
                }
                let idx = current_turn(&mut turns, &mut turn_seq);
                let ask = ThreadAsk {
                    id: request_id,
                    questions,
                    resolved: false,
                    answer: None,
                };
                turns[idx].items.push(ThreadEntry::Ask(ask));
                pending_ask = Some((idx, turns[idx].items.len() - 1));
            }
            "session.ended" => {
                session_ended = Some(SessionEnded {
                    reason: extra
                        .get("reason")
                        .and_then(|value| serde_json::from_value(value.clone()).ok())
                        .unwrap_or(StopReason::EndTurn),
                    message: str_field(extra, "message"),
                });
            }
            "note" | "lifecycle" => {
                let Some(text) = str_field(extra, "message") else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let tone = if str_field(extra, "noteKind").as_deref() == Some("provider-switch") {
                    NoteTone::Warning
                } else {
                    NoteTone::Dim
                };
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::Note(ThreadNote {
                    id: format!("v1:{}", event.seq),
                    text,
                    tone,
                }));
            }
            "error" | "session.error" => {
                let Some(text) = str_field(extra, "message") else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::Note(ThreadNote {
                    id: format!("v:{}", event.seq),
                    text,
                    tone: NoteTone::Danger,
                }));
            }
            "step-end" => {
                if str_field(extra, "status").as_deref() != Some("failed") {
                    continue;
                }
                let step = str_field(extra, "stepId").unwrap_or_else(|| "?".to_owned());
                let suffix = str_field(extra, "error")
                    .map(|error| format!(" — {error}"))
                    .unwrap_or_default();
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::Note(ThreadNote {
                    id: format!("v1:{}", event.seq),
                    text: format!("step {step} failed{suffix}"),
                    tone: NoteTone::Danger,
                }));
            }
            "check-output" => {
                let command = str_field(extra, "command").unwrap_or_else(|| "check".to_owned());
                let exit_code = extra.get("exitCode").and_then(Value::as_i64).unwrap_or(-1);
                let text = str_field(extra, "text").unwrap_or_default();
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::Item(UiItem::Tool(
                    coducktor_protocol::UiToolItem {
                        id: format!("v1:{}", event.seq),
                        name: "check".to_owned(),
                        tool_kind: coducktor_protocol::ToolKind::Execute,
                        title: format!("Ran {command}"),
                        status: if exit_code == 0 {
                            coducktor_protocol::ToolStatus::Completed
                        } else {
                            coducktor_protocol::ToolStatus::Failed
                        },
                        input: None,
                        output: Some(text),
                        error: None,
                        diffs: None,
                        locations: None,
                        exit_code: Some(exit_code as f64),
                        parent_item_id: None,
                    },
                )));
            }
            "provider-auth-required" => {
                let Some(provider) =
                    str_field(extra, "provider").and_then(|value| match value.as_str() {
                        "claude" => Some(AuthProvider::Claude),
                        "codex" => Some(AuthProvider::Codex),
                        "opencode" => Some(AuthProvider::OpenCode),
                        "pi" => Some(AuthProvider::Pi),
                        _ => None,
                    })
                else {
                    continue;
                };
                let Some(auth_failure_id) = str_field(extra, "authFailureId") else {
                    continue;
                };
                if auth_failure_id.is_empty() || auth_failure_id.len() > 128 {
                    continue;
                }
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::ProviderAuthRequired(
                    ThreadProviderAuthRequired {
                        id: format!("v1:{}", event.seq),
                        provider,
                        auth_failure_id,
                    },
                ));
            }
            "image" => {
                let Some(url) = str_field(extra, "url") else {
                    continue;
                };
                let idx = current_turn(&mut turns, &mut turn_seq);
                turns[idx].items.push(ThreadEntry::Image(ThreadImage {
                    id: format!("v1:{}", event.seq),
                    url,
                    name: str_field(extra, "name"),
                }));
            }
            // Pure engine control-flow / header material — never rendered in the body.
            "step-start" | "token-usage" | "cost" | "turn-end" | "done" | "session" => {}
            // `session.started`, `usage.updated`, `permission.*`: header/telemetry material.
            _ => {}
        }
    }

    let last_index = turns.len().saturating_sub(1);
    let turns: Vec<ThreadTurn> = turns
        .into_iter()
        .enumerate()
        .map(|(index, mut turn)| {
            let has_ask_card = turn
                .items
                .iter()
                .any(|item| matches!(item, ThreadEntry::Ask(_)));
            let provisional_ask = options.active_turn && index == last_index;
            let strip_ask = has_ask_card || provisional_ask;
            for item in &mut turn.items {
                if let ThreadEntry::Item(UiItem::Message(message)) = item
                    && message.role == coducktor_protocol::MessageRole::Assistant
                {
                    message.text = strip_done_marker(&message.text, strip_ask);
                }
            }
            turn
        })
        .collect();

    ThreadState {
        turns,
        session_ended,
    }
}

fn current_turn(turns: &mut Vec<ThreadTurn>, turn_seq: &mut u64) -> usize {
    if turns.is_empty() {
        *turn_seq += 1;
        turns.push(ThreadTurn {
            id: format!("turn-fallback-{turn_seq}"),
            turn_id: None,
            user_message: None,
            items: Vec::new(),
            plan_entries: None,
            completed: None,
        });
    }
    turns.len() - 1
}

fn item_id(item: &UiItem) -> &str {
    match item {
        UiItem::Message(item) => &item.id,
        UiItem::Reasoning(item) => &item.id,
        UiItem::Tool(item) => &item.id,
    }
}

fn str_field(extra: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    extra.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn valid_ask_question(value: &Value) -> Option<UiAskQuestion> {
    let object = value.as_object()?;
    if !object.get("header").is_some_and(Value::is_string) {
        return None;
    }
    let options: Vec<UiAskOption> = object
        .get("options")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|option| serde_json::from_value(option.clone()).ok())
        .collect();
    Some(UiAskQuestion {
        id: str_field(object, "id"),
        header: object.get("header")?.as_str()?.to_owned(),
        question: object
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        options,
        multi_select: object.get("multiSelect").and_then(Value::as_bool),
    })
}

/// The engine's turn-end markers (`DUCK:DONE`, `DUCK:MONITORING`) plus the in-band
/// task-reference marker lines (`DUCK:PR=` / `DUCK:ISSUE=` / `DUCK:TITLE=`). `strip_ask` gates
/// the `DUCK:ASK` strip on the turn actually holding an ask card — a marker whose card never
/// materialized stays visible as raw text.
fn strip_done_marker(text: &str, strip_ask: bool) -> String {
    let mut trailing = strip_trailing_marker(text, "DUCK:DONE");
    trailing = strip_trailing_marker(&trailing, "DUCK:MONITORING");
    if strip_ask {
        trailing = strip_trailing_ask_marker(&trailing);
    }
    if !trailing.contains("DUCK:") {
        return trailing;
    }
    trailing
        .lines()
        .filter(|line| !is_marker_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_trailing_marker(text: &str, marker: &str) -> String {
    let trimmed = text.trim_end();
    match trimmed.strip_suffix(marker) {
        Some(rest) => rest.trim_end().to_owned(),
        None => text.to_owned(),
    }
}

fn strip_trailing_ask_marker(text: &str) -> String {
    let trimmed = text.trim_end();
    if !trimmed.ends_with('}') {
        return trimmed.to_owned();
    }
    let Some(marker_at) = trimmed.rfind("DUCK:ASK") else {
        return trimmed.to_owned();
    };
    let after_marker = &trimmed[marker_at + "DUCK:ASK".len()..];
    let after_ws = after_marker.trim_start_matches([' ', '\t']);
    if after_ws.len() == after_marker.len() || !after_ws.starts_with('{') {
        return trimmed.to_owned();
    }
    trimmed[..marker_at].trim_end().to_owned()
}

fn is_marker_line(line: &str) -> bool {
    let trimmed = line.trim_end();
    if let Some(rest) = trimmed.strip_prefix("DUCK:PR=") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = trimmed.strip_prefix("DUCK:ISSUE=") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = trimmed.strip_prefix("DUCK:TITLE=") {
        return !rest.is_empty();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_protocol::{MessageRole, ToolStatus};
    use serde_json::json;

    fn event(seq: f64, event_type: &str, extra: Value) -> RunEvent {
        RunEvent {
            seq,
            ts: "2026-08-15T00:00:00Z".to_owned(),
            step_id: Some("step-1".to_owned()),
            event_type: event_type.to_owned(),
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn user_message_opens_a_turn_and_item_events_populate_it() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "do the thing"})),
            event(2.0, "turn.started", json!({"turnId": "t1"})),
            event(
                3.0,
                "item.started",
                json!({"item": {"kind": "message", "id": "m1", "role": "assistant", "text": "Sure"}}),
            ),
            event(
                4.0,
                "item.delta",
                json!({"itemId": "m1", "field": "text", "delta": ", on it."}),
            ),
            event(
                5.0,
                "turn.completed",
                json!({"turnId": "t1", "stopReason": "end_turn"}),
            ),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        assert_eq!(state.turns.len(), 1);
        let turn = &state.turns[0];
        assert_eq!(turn.user_message.as_ref().unwrap().text, "do the thing");
        assert_eq!(turn.items.len(), 1);
        let ThreadEntry::Item(UiItem::Message(message)) = &turn.items[0] else {
            panic!("expected a message item");
        };
        assert_eq!(message.text, "Sure, on it.");
        assert_eq!(message.role, MessageRole::Assistant);
        assert_eq!(
            turn.completed.as_ref().unwrap().stop_reason,
            StopReason::EndTurn
        );
    }

    #[test]
    fn legacy_runner_events_render_messages_reasoning_and_tool_lifecycle() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "inspect it"})),
            event(
                2.0,
                "reasoning",
                json!({"text": "I will inspect the file."}),
            ),
            event(
                3.0,
                "tool-call",
                json!({"id": "tool-1", "tool": "Read", "input": {"path": "README.md"}}),
            ),
            event(
                4.0,
                "tool-result",
                json!({"toolCallId": "tool-1", "result": "contents", "isError": false}),
            ),
            event(5.0, "text", json!({"text": "The file looks good."})),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        let items = &state.turns[0].items;
        assert!(items.iter().any(|entry| {
            matches!(
                entry,
                ThreadEntry::Item(UiItem::Message(message))
                    if message.text == "The file looks good."
            )
        }));
        assert!(items.iter().any(|entry| {
            matches!(
                entry,
                ThreadEntry::Item(UiItem::Reasoning(reasoning))
                    if reasoning.text == "I will inspect the file."
            )
        }));
        assert!(items.iter().any(|entry| {
            matches!(
                entry,
                ThreadEntry::Item(UiItem::Tool(tool))
                    if tool.id == "tool-1"
                        && tool.status == coducktor_protocol::ToolStatus::Completed
                        && tool.output.as_deref() == Some("contents")
            )
        }));
    }

    #[test]
    fn plan_updated_is_full_replacement_and_latest_wins_across_turns() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "go"})),
            event(
                2.0,
                "plan.updated",
                json!({"entries": [{"content": "step one", "status": "pending"}]}),
            ),
            event(3.0, "user-message", json!({"text": "more"})),
            event(4.0, "plan.updated", json!({"entries": []})),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        assert_eq!(latest_plan_entries(&state), Some([].as_slice()));
    }

    #[test]
    fn ask_requested_opens_a_card_and_the_next_user_message_resolves_it() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "go"})),
            event(
                2.0,
                "ask.requested",
                json!({
                    "requestId": "ask-1",
                    "questions": [{"header": "PICK", "question": "which one?", "options": [{"label": "a"}]}],
                }),
            ),
            event(3.0, "user-message", json!({"text": "a"})),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        let ThreadEntry::Ask(ask) = &state.turns[0].items[0] else {
            panic!("expected an ask card");
        };
        assert!(ask.resolved);
        assert_eq!(ask.answer.as_deref(), Some("a"));
    }

    #[test]
    fn done_and_monitoring_markers_are_stripped_from_assistant_text() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "go"})),
            event(
                2.0,
                "item.completed",
                json!({"item": {"kind": "message", "id": "m1", "role": "assistant", "text": "All set.\nDUCK:DONE"}}),
            ),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        let ThreadEntry::Item(UiItem::Message(message)) = &state.turns[0].items[0] else {
            panic!("expected a message item");
        };
        assert_eq!(message.text, "All set.");
    }

    #[test]
    fn a_malformed_event_costs_one_event_not_the_fold() {
        let events = vec![
            event(1.0, "user-message", json!({"text": "go"})),
            event(2.0, "item.started", json!({"item": {"kind": "message"}})),
            event(
                3.0,
                "item.started",
                json!({"item": {"kind": "tool", "id": "t1", "name": "Bash", "toolKind": "execute", "title": "Run", "status": "running"}}),
            ),
        ];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        assert_eq!(
            state.turns[0].items.len(),
            1,
            "the malformed item is dropped, not the whole turn"
        );
        assert!(matches!(
            state.turns[0].items[0],
            ThreadEntry::Item(UiItem::Tool(_))
        ));
    }

    #[test]
    fn check_output_renders_as_an_execute_tool_card() {
        let events = vec![event(
            1.0,
            "check-output",
            json!({"command": "cargo test", "exitCode": 1, "text": "FAILED"}),
        )];
        let state = reduce_thread(&events, ThreadReduceOptions::default());
        let ThreadEntry::Item(UiItem::Tool(tool)) = &state.turns[0].items[0] else {
            panic!("expected a tool item");
        };
        assert_eq!(tool.status, ToolStatus::Failed);
        assert_eq!(tool.title, "Ran cargo test");
    }

    #[test]
    fn thread_footer_reads_status_into_a_dim_or_danger_strip() {
        use coducktor_contract::RunStatus;
        assert_eq!(thread_footer(RunStatus::Running, None), None);
        assert_eq!(
            thread_footer(RunStatus::Waiting, None),
            Some(ThreadFooter::Waiting)
        );
        assert!(matches!(
            thread_footer(RunStatus::Failed, Some("boom")),
            Some(ThreadFooter::Closed { danger: true, .. })
        ));
    }
}
