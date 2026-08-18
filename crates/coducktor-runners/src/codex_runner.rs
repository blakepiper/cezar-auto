//! `AgentSession` over `codex app-server` — the same JSON-RPC 2.0 (newline-delimited) transport
//! the VS Code extension and desktop app use.
//!
//! Auth is the host's logged-in ChatGPT/Codex session (or `CODEX_API_KEY`). The agent runs
//! autonomously via `sandbox: danger-full-access` + `approvalPolicy: never`; `DUCK_CODEX_NETWORK=0`
//! retains the previous network-blocked `workspace-write` sandbox as an explicit restriction.
//! Codex has no per-tool allowlist, so `spec.allowed_tools`/`bash_allowlist` are ignored. Pasted
//! images are forwarded as app-server `image` user-input items on opening and follow-up turns.
//!
//! # Architecture notes
//!
//! The turn-scoped adapter has two consequences specific to Codex's richer protocol:
//!
//! - **Bootstrap is deferred into the first `turn()` call.** The session has no event sink until
//!   a trait method hands it one, so [`open_codex_session`] only spawns the process; the first
//!   call to [`AgentSession::turn`] performs the `initialize` -> `thread/start` -> `turn/start`
//!   handshake before reading toward the first turn's end.
//! - **`item/tool/requestUserInput` ends the read loop.** Receiving this request stops
//!   [`CodexSession::drive`] with [`StopReason::UserInputRequested`], producing a `Waiting`
//!   outcome with `decision: Ask`. The next `send_message` answers it via RPC instead of starting
//!   a new turn, then resumes reading like any other follow-up.
//!
//! The session does not need a separate signal-termination marker: no call can signal it while a
//! read loop is in flight, so there is no ambiguity to resolve.
//!
//! A failed `turn/steer` or `turn/start` RPC is returned as a hard `Err`, matching bootstrap RPC
//! failures; a clean failure is preferable to a session that silently stops producing events.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use coducktor_contract::Runner;
use coducktor_core::runs::ask::{self, AskQuestion};
use coducktor_core::workflows::run::{
    AgentSession, EventInput, PromptImage, SessionOutcome, SessionReport, TurnMarkerDecision,
    decide_turn_marker,
};
use serde_json::{Map, Value, json};

use crate::agent_runner::{
    AgentRunSpec, ContentBlock, prepend_system_prompt, reasoning_effort_str,
};
use crate::child_process::{ChildProcess, NextLine, SpawnConfig};
use crate::claude_runner::{
    DEFAULT_RUN_TIMEOUT_MS, EOF_KILL_GRACE_MS, EOF_TERM_GRACE_MS, KILL_GRACE_MS,
};
use crate::v1_text_coalescer::V1TextCoalescer;
use crate::wire::json_string;

/// ThreadItem `type`s that are conversation text, not tool activity.
const NON_TOOL_ITEMS: &[&str] = &["agentMessage", "userMessage", "reasoning", "plan"];

const REASONING_SUMMARIES: &[&str] = &["auto", "concise", "detailed", "none"];

/// Where to find the codex binary. Production wiring resolves `program`/`prefix_args` from
/// `DUCK_CODEX_BIN` is resolved by the session factory; tests point `program` at `node`
/// with `prefix_args: vec![mock_script_path]`. `app-server` is always appended as the final arg
/// regardless — matching `spawnCodexAppServer`'s own `nodeSpawn(bin, ['app-server'], ...)`.
#[derive(Debug, Clone)]
pub struct CodexSpawnConfig {
    pub program: String,
    pub prefix_args: Vec<String>,
    pub eof_term_grace: Duration,
    pub eof_kill_grace: Duration,
    pub kill_grace: Duration,
}

impl Default for CodexSpawnConfig {
    fn default() -> Self {
        Self {
            program: "codex".to_owned(),
            prefix_args: Vec::new(),
            eof_term_grace: Duration::from_millis(EOF_TERM_GRACE_MS),
            eof_kill_grace: Duration::from_millis(EOF_KILL_GRACE_MS),
            kill_grace: Duration::from_millis(KILL_GRACE_MS),
        }
    }
}

fn wrap_spawn_error(error: &io::Error, program: &str) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!(
            "`{program}` not found on PATH — install the Codex CLI (npm i -g @openai/codex) and run `codex` once to log in"
        )
    } else {
        error.to_string()
    }
}

/// `CLAUDE_CODE_USE_BEDROCK`-style toggle: full access is the shared `auto` preset across every
/// backend; `DUCK_CODEX_NETWORK=0` remains the backwards-compatible explicit sandbox opt-out.
fn resolve_sandbox(env: &BTreeMap<String, String>) -> &'static str {
    if env.get("DUCK_CODEX_NETWORK").map(String::as_str) == Some("0") {
        "workspace-write"
    } else {
        "danger-full-access"
    }
}

/// The reasoning-summary override sent on `turn/start`. Defaults to `auto`; `DUCK_CODEX_REASONING`
/// overrides it (`auto`/`concise`/`detailed`, or `none` to opt out); an unrecognized value falls
/// back to `auto`.
fn resolve_reasoning_summary(env: &BTreeMap<String, String>) -> String {
    match env
        .get("DUCK_CODEX_REASONING")
        .map(|value| value.trim().to_lowercase())
    {
        Some(value) if REASONING_SUMMARIES.contains(&value.as_str()) => value,
        _ => "auto".to_owned(),
    }
}

struct PendingUserInput {
    rpc_id: Value,
    questions: Vec<AskQuestion>,
}

/// A live `codex app-server` session driving a single thread. Implements [`AgentSession`].
pub struct CodexSession {
    process: ChildProcess,
    spec: AgentRunSpec,
    next_id: u64,
    thread_id: Option<String>,
    active_turn_id: Option<String>,
    pending_user_input: Option<PendingUserInput>,
    /// Whether stdin is still open for this session.
    open: bool,
    /// 0 disables the wall-clock kill switch entirely (interactive sessions).
    timeout_ms: u64,
    sandbox: &'static str,
    reasoning_summary: String,
}

/// Spawn a codex app-server process. Unlike claude's `open_claude_session`, this does not talk to
/// the process yet — see the module doc's note on why bootstrap is deferred to the first `turn()`.
pub fn open_codex_session(
    config: &CodexSpawnConfig,
    spec: AgentRunSpec,
    host_env: &BTreeMap<String, String>,
) -> Result<CodexSession, String> {
    let mut args = config.prefix_args.clone();
    args.push("app-server".to_owned());
    let mut process = ChildProcess::spawn(
        &SpawnConfig {
            program: config.program.clone(),
            args,
            eof_term_grace: config.eof_term_grace,
            eof_kill_grace: config.eof_kill_grace,
            kill_grace: config.kill_grace,
        },
        Runner::Codex,
        &spec.cwd,
        &spec.env,
        host_env,
    )
    .map_err(|error| wrap_spawn_error(&error, &config.program))?;
    process.set_cancellation(spec.cancellation.clone());

    let timeout_ms = spec.timeout_ms.unwrap_or(DEFAULT_RUN_TIMEOUT_MS);
    let sandbox = resolve_sandbox(host_env);
    let reasoning_summary = resolve_reasoning_summary(host_env);

    Ok(CodexSession {
        process,
        spec,
        next_id: 1,
        thread_id: None,
        active_turn_id: None,
        pending_user_input: None,
        open: true,
        timeout_ms,
        sandbox,
        reasoning_summary,
    })
}

#[derive(Default)]
struct TurnState {
    text_chunks: Vec<String>,
    coalescer: V1TextCoalescer,
    /// The latest `(input, output, total)` from a `thread/tokenUsage/updated` notification's
    /// `tokenUsage.last` — already scoped to the current turn by the app-server itself, unlike
    /// `tokenUsage.total` (whole-thread cumulative, which this session never reports).
    usage: Option<(f64, f64, f64)>,
}

#[derive(Clone, Copy)]
enum StopAt {
    Response(u64),
    TurnEnd,
}

enum StopReason {
    RpcOk(Value),
    TurnEnded,
    UserInputRequested,
    ChannelClosed,
}

fn codex_error_text(error: &Value) -> String {
    if let Some(message) = error.get("message") {
        return match message {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }
    match error {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn thread_id_of(value: &Value) -> Option<String> {
    value
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("threadId").and_then(Value::as_str))
        .map(str::to_owned)
}

fn turn_id_of(value: &Value) -> Option<String> {
    value
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("turnId").and_then(Value::as_str))
        .map(str::to_owned)
}

/// Cumulative-vs-per-turn note: `params.tokenUsage.last` is already the CURRENT turn's usage
/// (the app-server's own scoping), not a running session total — no delta bookkeeping needed.
fn turn_usage_last(params: &Value) -> Option<(f64, f64, f64)> {
    let last = params.get("tokenUsage")?.get("last")?;
    let input = last.get("inputTokens").and_then(Value::as_f64)?;
    let output = last.get("outputTokens").and_then(Value::as_f64)?;
    let total = last
        .get("totalTokens")
        .and_then(Value::as_f64)
        .unwrap_or(input + output);
    Some((input, output, total))
}

/// Build a candidate request shape from the app-server's question wire format, then re-validate
/// it through the same strict
/// schema (`coducktor_core::runs::ask`) a text-marker ask uses — one source of truth for what a
/// valid ask looks like, regardless of which backend produced it.
fn codex_ask_questions(value: &Value) -> Option<Vec<AskQuestion>> {
    let items = value.as_array()?;
    if items.is_empty() || items.len() > 4 {
        return None;
    }
    let mut questions = Vec::with_capacity(items.len());
    for raw in items {
        let question = raw.as_object()?;
        if question.get("isSecret").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let id = question.get("id").and_then(Value::as_str)?;
        let header = question.get("header").and_then(Value::as_str)?;
        let prompt = question.get("question").and_then(Value::as_str)?;
        let raw_options = question.get("options").and_then(Value::as_array)?;
        let options: Vec<Value> = raw_options
            .iter()
            .filter_map(|option| {
                let record = option.as_object()?;
                let label = record.get("label").and_then(Value::as_str)?;
                let mut built = Map::new();
                built.insert("label".to_owned(), Value::String(label.to_owned()));
                if let Some(description) = record.get("description").and_then(Value::as_str) {
                    built.insert(
                        "description".to_owned(),
                        Value::String(description.to_owned()),
                    );
                }
                Some(Value::Object(built))
            })
            .collect();
        questions.push(json!({
            "id": id,
            "header": header,
            "question": prompt,
            "options": options,
            "multiSelect": false,
        }));
    }
    ask::parse_ask_request(&json!({ "questions": questions })).map(|request| request.questions)
}

/// Build a structured `Header: value` line per question, or (when there is exactly one question
/// and no structured line matched) the
/// whole free-text reply as that question's answer.
fn user_input_answers(questions: &[AskQuestion], text: &str) -> Value {
    let lines: Vec<&str> = text.lines().collect();
    let has_structured_answer = questions.iter().any(|question| {
        let prefix = format!("{}:", question.header);
        lines.iter().any(|line| line.starts_with(&prefix))
    });
    let mut answers = Map::new();
    for (index, question) in questions.iter().enumerate() {
        let prefix = format!("{}:", question.header);
        let matching = lines.iter().find(|line| line.starts_with(&prefix));
        let raw = match matching {
            Some(line) => line[prefix.len()..].trim().to_owned(),
            None if !has_structured_answer && index == 0 => text.trim().to_owned(),
            None => String::new(),
        };
        let values: Vec<Value> = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| Value::String(part.to_owned()))
                .collect()
        };
        let key = question.id.clone().unwrap_or_else(|| index.to_string());
        answers.insert(key, json!({ "answers": values }));
    }
    Value::Object(answers)
}

impl CodexSession {
    fn write_request(&mut self, id: u64, method: &str, params: Value) -> Result<(), String> {
        self.process
            .write_line(&json!({"id": id, "method": method, "params": params}).to_string())
    }

    fn write_notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.process
            .write_line(&json!({"method": method, "params": params}).to_string())
    }

    fn write_response(&mut self, id: Value, result: Value) -> Result<(), String> {
        self.process
            .write_line(&json!({"id": id, "result": result}).to_string())
    }

    fn write_response_error(&mut self, id: Value, code: i32, message: &str) -> Result<(), String> {
        self.process
            .write_line(&json!({"id": id, "error": {"code": code, "message": message}}).to_string())
    }

    fn is_foreign_thread_turn(&self, params: &Value) -> bool {
        let event_thread_id = params.get("threadId").and_then(Value::as_str);
        matches!(
            (event_thread_id, self.thread_id.as_deref()),
            (Some(event_id), Some(our_id)) if event_id != our_id
        )
    }

    /// The shared read/dispatch loop: every RPC roundtrip (bootstrap and mid-turn alike) and every
    /// "read until the turn ends" wait goes through this. Notifications are dispatched as they
    /// arrive regardless of what we're waiting for — a `turn/steer` ack and the item stream for
    /// the turn it just re-armed can interleave on the wire, and both need to reach `on_event`.
    fn drive(
        &mut self,
        stop_at: StopAt,
        deadline: Option<Instant>,
        turn: &mut TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<StopReason, String> {
        loop {
            let line = match self.process.next_line(deadline) {
                Ok(NextLine::Line(line)) => line,
                Ok(NextLine::Closed) => return Ok(StopReason::ChannelClosed),
                Err(_timed_out) => {
                    self.open = false;
                    self.process.escalate_after_timeout();
                    let minutes = (self.timeout_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
                    let message =
                        format!("codex app-server timed out after {minutes}m and was killed");
                    on_event(EventInput::new("error").field("message", message.clone()))
                        .map_err(|error| error.to_string())?;
                    return Err(message);
                }
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Not JSON-RPC — skip silently.
            let Ok(msg) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            let id = msg.get("id").cloned();
            let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
            let has_result_or_error = msg.get("result").is_some() || msg.get("error").is_some();

            if method.is_none() && has_result_or_error {
                if let (Some(id_value), StopAt::Response(want)) = (&id, stop_at)
                    && id_value.as_u64() == Some(want)
                {
                    if let Some(error) = msg.get("error") {
                        return Err(codex_error_text(error));
                    }
                    return Ok(StopReason::RpcOk(
                        msg.get("result").cloned().unwrap_or(Value::Null),
                    ));
                }
                continue; // a stale/unmatched response — this session never has more than one in flight
            }

            let Some(method) = method else { continue };

            if method == "item/tool/requestUserInput"
                && let Some(id) = id.clone()
            {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                match params.get("questions").and_then(codex_ask_questions) {
                    Some(questions) => {
                        self.pending_user_input = Some(PendingUserInput {
                            rpc_id: id,
                            questions,
                        });
                        return Ok(StopReason::UserInputRequested);
                    }
                    None => {
                        self.write_response_error(
                            id,
                            -32602,
                            "unsupported or malformed requestUserInput payload",
                        )?;
                        continue;
                    }
                }
            }

            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            // A sub-agent child thread's turn lifecycle must reach neither channel (#600): a
            // spawned skill's own turn/completed would otherwise end OUR turn. Item events still
            // flow (checked below, not here), so nested sub-agent activity keeps rendering.
            if matches!(
                method.as_str(),
                "turn/started" | "turn/completed" | "turn/failed"
            ) && self.is_foreign_thread_turn(&params)
            {
                continue;
            }
            let ended = self.handle_notification(&method, &params, turn, on_event)?;
            if ended && matches!(stop_at, StopAt::TurnEnd) {
                return Ok(StopReason::TurnEnded);
            }
        }
    }

    fn call_rpc(
        &mut self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
        turn: &mut TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_request(id, method, params)?;
        match self.drive(StopAt::Response(id), deadline, turn, on_event)? {
            StopReason::RpcOk(value) => Ok(value),
            StopReason::ChannelClosed => Err(format!(
                "codex app-server exited before responding to {method}"
            )),
            StopReason::TurnEnded | StopReason::UserInputRequested => {
                unreachable!("drive only returns these when stop_at = TurnEnd")
            }
        }
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: &Value,
        turn: &mut TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<bool, String> {
        match method {
            "turn/started" => {
                if let Some(turn_id) = turn_id_of(params) {
                    self.active_turn_id = Some(turn_id);
                }
                Ok(false)
            }
            "item/agentMessage/delta" => {
                let delta = params.get("delta").and_then(Value::as_str).unwrap_or("");
                if !delta.is_empty() {
                    let item_id = params.get("itemId").and_then(Value::as_str);
                    turn.coalescer.append(item_id, delta);
                }
                Ok(false)
            }
            "item/started" => {
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                if let Some(item_type) = item.get("type").and_then(Value::as_str)
                    && !NON_TOOL_ITEMS.contains(&item_type)
                {
                    let id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            self.next_id += 1;
                            format!("item-{}", self.next_id)
                        });
                    on_event(
                        EventInput::new("tool-call")
                            .field("id", &id)
                            .field("tool", item_type)
                            .field("input", item.clone()),
                    )
                    .map_err(|error| error.to_string())?;
                }
                Ok(false)
            }
            "item/completed" => {
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                let item_type = item.get("type").and_then(Value::as_str).map(str::to_owned);
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if item_type.as_deref() == Some("agentMessage") {
                    let snapshot = item.get("text").and_then(Value::as_str);
                    let coalesced = turn
                        .coalescer
                        .complete(if id.is_empty() { None } else { Some(&id) }, snapshot);
                    if let Some(text) = coalesced {
                        turn.text_chunks.push(text.clone());
                        on_event(EventInput::new("text").field("text", text))
                            .map_err(|error| error.to_string())?;
                    }
                } else if item_type.as_deref() == Some("reasoning") {
                    if let Some(text) = item
                        .get("text")
                        .or_else(|| item.get("summary"))
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        on_event(EventInput::new("reasoning").field("text", text))
                            .map_err(|error| error.to_string())?;
                    }
                } else if let Some(item_type) = item_type.as_deref()
                    && !NON_TOOL_ITEMS.contains(&item_type)
                    && !id.is_empty()
                {
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let is_error = status.contains("error") || status.contains("failed");
                    on_event(
                        EventInput::new("tool-result")
                            .field("toolCallId", &id)
                            .field("result", json_string(&item))
                            .field("isError", is_error),
                    )
                    .map_err(|error| error.to_string())?;
                }
                Ok(false)
            }
            "thread/tokenUsage/updated" => {
                if let Some(usage) = turn_usage_last(params) {
                    turn.usage = Some(usage);
                    on_event(EventInput::new("token-usage").field("tokensUsed", usage.2))
                        .map_err(|error| error.to_string())?;
                }
                Ok(false)
            }
            "turn/completed" | "turn/failed" => {
                self.pending_user_input = None;
                self.active_turn_id = None;
                // An interrupted/failed item never sees item/completed — surface its partial
                // prose before the turn boundary (marker detection reads it there).
                for text in turn.coalescer.flush() {
                    turn.text_chunks.push(text.clone());
                    on_event(EventInput::new("text").field("text", text))
                        .map_err(|error| error.to_string())?;
                }
                if method == "turn/failed" {
                    let message = params
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("codex turn failed");
                    on_event(EventInput::new("error").field("message", message))
                        .map_err(|error| error.to_string())?;
                }
                on_event(EventInput::new("turn-end")).map_err(|error| error.to_string())?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn bootstrap_and_first_turn(
        &mut self,
        deadline: Option<Instant>,
        turn: &mut TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        self.call_rpc(
            "initialize",
            json!({
                "clientInfo": {"name": "coducktor", "title": "coducktor", "version": "0.1.0"},
                "capabilities": {"experimentalApi": true},
            }),
            deadline,
            turn,
            on_event,
        )?;
        self.write_notify("initialized", json!({}))?;

        let mut overrides = Map::new();
        if let Some(model) = &self.spec.model {
            overrides.insert("model".to_owned(), json!(model));
        }
        if let Some(effort) = self.spec.reasoning_effort {
            overrides.insert("effort".to_owned(), json!(reasoning_effort_str(effort)));
        }
        overrides.insert("cwd".to_owned(), json!(self.spec.cwd.to_string_lossy()));
        overrides.insert("sandbox".to_owned(), json!(self.sandbox));
        overrides.insert("approvalPolicy".to_owned(), json!("never"));

        let result = if self.spec.resume && self.spec.session_id.is_some() {
            let mut params = overrides;
            params.insert(
                "threadId".to_owned(),
                json!(self.spec.session_id.clone().unwrap()),
            );
            self.call_rpc(
                "thread/resume",
                Value::Object(params),
                deadline,
                turn,
                on_event,
            )?
        } else {
            self.call_rpc(
                "thread/start",
                Value::Object(overrides),
                deadline,
                turn,
                on_event,
            )?
        };
        self.thread_id = thread_id_of(&result).or_else(|| self.spec.session_id.clone());

        if let Some(thread_id) = self.thread_id.clone() {
            on_event(EventInput::new("session").field("sessionId", thread_id))
                .map_err(|error| error.to_string())?;
        }

        // The system prompt has no dedicated app-server field, so it rides along as a leading
        // block of the opening message.
        let first_text =
            prepend_system_prompt(self.spec.system_prompt.as_deref(), &self.spec.user_prompt);
        let image_urls = self
            .spec
            .images
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image { source } => {
                    Some(format!("data:{};base64,{}", source.media_type, source.data))
                }
                ContentBlock::Text { .. } => None,
            })
            .collect::<Vec<_>>();
        self.start_or_steer_turn(&first_text, &image_urls, deadline, turn, on_event)
    }

    fn start_or_steer_turn(
        &mut self,
        text: &str,
        image_urls: &[String],
        deadline: Option<Instant>,
        turn: &mut TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Ok(());
        };
        let input = codex_user_input(text, image_urls);
        if let Some(active_turn_id) = self.active_turn_id.clone() {
            self.call_rpc(
                "turn/steer",
                json!({"threadId": thread_id, "input": input, "expectedTurnId": active_turn_id}),
                deadline,
                turn,
                on_event,
            )?;
            return Ok(());
        }
        // Ask for reasoning summaries; without this the model runs with no summary and the
        // reasoning thread stays empty even though the mapper/UI can render it. The override
        // persists for this and every subsequent turn, so seeding it here is enough.
        let result = self.call_rpc(
            "turn/start",
            json!({"threadId": thread_id, "input": input, "summary": self.reasoning_summary}),
            deadline,
            turn,
            on_event,
        )?;
        self.active_turn_id = turn_id_of(&result).or_else(|| self.active_turn_id.clone());
        Ok(())
    }

    fn build_report(
        &self,
        turn_text: String,
        usage: Option<(f64, f64, f64)>,
        decision: TurnMarkerDecision,
    ) -> SessionReport {
        SessionReport {
            session_id: self.thread_id.clone(),
            tokens_used: usage.map(|(_, _, total)| total).unwrap_or(0.0),
            input_tokens: usage.map(|(input, _, _)| input),
            output_tokens: usage.map(|(_, output, _)| output),
            cost_usd: None,
            turn_text,
            decision: Some(decision),
            plan_entries: None,
        }
    }

    fn finalize_turn(&self, turn: TurnState) -> Result<SessionOutcome, String> {
        let turn_text = turn.text_chunks.join("\n").trim().to_owned();
        let valid_ask = ask::parse_ask_marker(&turn_text).is_some();
        let decision = decide_turn_marker(&turn_text, self.open, valid_ask);
        let report = self.build_report(turn_text, turn.usage, decision);
        Ok(if decision == TurnMarkerDecision::Done {
            SessionOutcome::Completed(report)
        } else {
            SessionOutcome::Waiting(report)
        })
    }

    /// A structured ask outranks text-marker detection entirely — it is a stronger signal than a
    /// trailing marker could ever be.
    fn finalize_ask(&self, turn: TurnState) -> Result<SessionOutcome, String> {
        let turn_text = turn.text_chunks.join("\n").trim().to_owned();
        let report = self.build_report(turn_text, turn.usage, TurnMarkerDecision::Ask);
        Ok(SessionOutcome::Waiting(report))
    }

    fn finalize_after_channel_closed(
        &mut self,
        turn: TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        let exit_code = self.process.wait_for_exit();
        if let Some(code) = exit_code
            && code != 0
        {
            let stderr = self.process.take_stderr_tail();
            let detail = if stderr.is_empty() {
                String::new()
            } else {
                format!(" — {stderr}")
            };
            let message = format!("codex app-server exited with code {code}{detail}");
            on_event(EventInput::new("error").field("message", message.clone()))
                .map_err(|error| error.to_string())?;
            return Err(message);
        }
        self.finalize_turn(turn)
    }

    fn finish_turn_reading(
        &mut self,
        deadline: Option<Instant>,
        mut turn: TurnState,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        match self.drive(StopAt::TurnEnd, deadline, &mut turn, on_event)? {
            StopReason::TurnEnded => self.finalize_turn(turn),
            StopReason::UserInputRequested => self.finalize_ask(turn),
            StopReason::ChannelClosed => self.finalize_after_channel_closed(turn, on_event),
            StopReason::RpcOk(_) => {
                unreachable!("drive only returns RpcOk when stop_at = Response")
            }
        }
    }
}

fn codex_user_input(text: &str, image_urls: &[String]) -> Vec<Value> {
    let mut input = Vec::with_capacity(image_urls.len() + usize::from(!text.is_empty()));
    for url in image_urls {
        input.push(json!({"type": "image", "url": url}));
    }
    if !text.is_empty() {
        input.push(json!({"type": "text", "text": text, "text_elements": []}));
    }
    input
}

impl AgentSession for CodexSession {
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if !self.open {
            return Err("session is closed".to_owned());
        }
        let deadline =
            (self.timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(self.timeout_ms));
        let mut turn = TurnState::default();
        self.bootstrap_and_first_turn(deadline, &mut turn, on_event)?;
        self.finish_turn_reading(deadline, turn, on_event)
    }

    fn send_message(
        &mut self,
        prompt: &str,
        images: &[PromptImage],
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if !self.open {
            return Err("session does not accept follow-up messages".to_owned());
        }
        let deadline =
            (self.timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(self.timeout_ms));
        let mut turn = TurnState::default();
        if let Some(pending) = self.pending_user_input.take() {
            let answers = user_input_answers(&pending.questions, prompt);
            self.write_response(pending.rpc_id, json!({ "answers": answers }))?;
        } else {
            let image_urls = images.iter().map(PromptImage::data_url).collect::<Vec<_>>();
            self.start_or_steer_turn(prompt, &image_urls, deadline, &mut turn, on_event)?;
        }
        self.finish_turn_reading(deadline, turn, on_event)
    }

    fn finish(
        &mut self,
        _on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if self.open {
            self.open = false;
            self.process.close_stdin(); // delivers EOF to the app-server
            self.process.escalate_after_eof();
        }
        self.process.wait_for_exit();
        Ok(SessionOutcome::Completed(SessionReport::default()))
    }

    fn cancel(&mut self) {
        self.open = false;
        // Best-effort graceful cancel of the in-flight turn (fire-and-forget, matching TS's own
        // `.catch(() => undefined)`; there is no event sink here to dispatch its response
        // through), then a hard stop.
        if let Some(thread_id) = self.thread_id.clone()
            && let Some(active_turn_id) = self.active_turn_id.clone()
        {
            let _ = self.write_request(
                0,
                "turn/interrupt",
                json!({"threadId": thread_id, "turnId": active_turn_id}),
            );
        }
        if !self.process.has_exited() {
            self.process.signal_term();
        }
    }

    fn session_id(&self) -> Option<String> {
        self.thread_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mock_script() -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../fixtures/codex/mock-codex-app-server.mjs");
        path.canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn node_config() -> CodexSpawnConfig {
        CodexSpawnConfig {
            program: "node".to_owned(),
            prefix_args: vec![mock_script()],
            ..Default::default()
        }
    }

    fn spec_for(cwd: &std::path::Path, user_prompt: &str) -> AgentRunSpec {
        AgentRunSpec {
            user_prompt: user_prompt.to_owned(),
            cwd: cwd.to_path_buf(),
            timeout_ms: Some(0),
            ..Default::default()
        }
    }

    fn run_turn(session: &mut CodexSession) -> (Result<SessionOutcome, String>, Vec<EventInput>) {
        let mut events = Vec::new();
        let result = session.turn(&mut |event| {
            events.push(event);
            Ok(())
        });
        (result, events)
    }

    #[test]
    fn resolve_sandbox_defaults_to_full_access() {
        assert_eq!(resolve_sandbox(&BTreeMap::new()), "danger-full-access");
        let mut env = BTreeMap::new();
        env.insert("DUCK_CODEX_NETWORK".to_owned(), "0".to_owned());
        assert_eq!(resolve_sandbox(&env), "workspace-write");
    }

    #[test]
    fn resolve_reasoning_summary_falls_back_to_auto() {
        assert_eq!(resolve_reasoning_summary(&BTreeMap::new()), "auto");
        let mut env = BTreeMap::new();
        env.insert("DUCK_CODEX_REASONING".to_owned(), "DETAILED".to_owned());
        assert_eq!(resolve_reasoning_summary(&env), "detailed");
        env.insert("DUCK_CODEX_REASONING".to_owned(), "nonsense".to_owned());
        assert_eq!(resolve_reasoning_summary(&env), "auto");
    }

    #[test]
    fn a_first_turn_streams_the_expected_events_and_parks_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "check the working tree");
        let mut session = open_codex_session(&config, run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        let outcome = outcome.expect("first turn should complete");

        let event_types: Vec<&str> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        assert!(event_types.contains(&"session"));
        assert!(event_types.contains(&"text"));
        assert!(event_types.contains(&"tool-call"));
        assert!(event_types.contains(&"tool-result"));
        assert!(event_types.contains(&"token-usage"));
        assert!(event_types.contains(&"turn-end"));

        match outcome {
            SessionOutcome::Waiting(report) => {
                assert!(report.turn_text.contains("Checking the working tree."));
                assert_eq!(report.tokens_used, 1500.0);
                assert_eq!(report.input_tokens, Some(1200.0));
                assert_eq!(report.output_tokens, Some(300.0));
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_failed_turn_surfaces_an_error_event_and_settles_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "mock:turn-failed");
        let mut session = open_codex_session(&config, run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        outcome.expect("turn/failed settles the session with an error event, not a hard Err");
        assert!(events.iter().any(|event| {
            event.event_type == "error"
                && event.extra.get("message").and_then(Value::as_str) == Some("model unavailable")
        }));
        assert!(events.iter().any(|event| event.event_type == "turn-end"));
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_native_ask_request_parks_and_the_answer_completes_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "mock:native-codex-ask");
        let mut session = open_codex_session(&config, run_spec, &BTreeMap::new()).unwrap();
        let (outcome, _events) = run_turn(&mut session);
        match outcome.unwrap() {
            SessionOutcome::Waiting(report) => {
                assert_eq!(report.decision, Some(TurnMarkerDecision::Ask))
            }
            other => panic!("expected Waiting/Ask, got {other:?}"),
        }
        let outcome = session
            .send_message("Library: Vitest", &[], &mut |_| Ok(()))
            .unwrap();
        assert!(matches!(
            outcome,
            SessionOutcome::Waiting(_) | SessionOutcome::Completed(_)
        ));
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn codex_user_input_places_data_url_images_before_text() {
        assert_eq!(
            codex_user_input("inspect", &["data:image/png;base64,AQID".to_owned()]),
            vec![
                json!({"type": "image", "url": "data:image/png;base64,AQID"}),
                json!({"type": "text", "text": "inspect", "text_elements": []}),
            ]
        );
    }

    #[test]
    fn a_child_threads_turn_lifecycle_does_not_end_the_parent_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "mock:child-turn");
        let mut session = open_codex_session(&config, run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        match outcome.unwrap() {
            SessionOutcome::Waiting(report) => {
                assert!(
                    report
                        .turn_text
                        .contains("Still working after the sub-agent.")
                );
            }
            other => panic!("expected Waiting, got {other:?}"),
        }
        // The child thread's own commandExecution item still rendered (#600: only turn
        // lifecycle is filtered — item events keep flowing).
        assert!(events.iter().any(|event| event.event_type == "tool-call"));
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn finish_escalates_through_sigterm_against_an_app_server_that_ignores_eof() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = node_config();
        config.eof_term_grace = Duration::from_millis(80);
        config.eof_kill_grace = Duration::from_millis(200);
        let mut run_spec = spec_for(dir.path(), "anything");
        run_spec
            .env
            .insert("MOCK_CODEX_IGNORE_EOF".to_owned(), "1".to_owned());
        let mut session = open_codex_session(&config, run_spec, &BTreeMap::new()).unwrap();
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn spawn_failure_reports_a_friendly_missing_binary_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = CodexSpawnConfig {
            program: "coducktor-test-nonexistent-binary-xyz".to_owned(),
            ..Default::default()
        };
        let run_spec = spec_for(dir.path(), "hi");
        let error = match open_codex_session(&config, run_spec, &BTreeMap::new()) {
            Err(error) => error,
            Ok(_) => panic!("expected spawn to fail"),
        };
        assert!(
            error.contains("not found on PATH"),
            "unexpected message: {error}"
        );
    }
}
