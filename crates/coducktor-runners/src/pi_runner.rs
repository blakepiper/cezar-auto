//! `AgentSession` over pi's documented RPC mode.
//!
//! Contract: <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/rpc.md>.
//! pi has its own command/event vocabulary (not claude stream-json, not codex's JSON-RPC
//! app-server); auth is the host's configured provider (`pi` reads its own credentials store),
//! and there is no per-tool prefix-allowlist syntax the way claude's `Bash(<prefix>:*)` has — a
//! `bash_allowlist` fails the whole `Bash` tool closed rather than narrowing it (`pi_tools`).
//!
//! # Architecture notes
//!
//! The session reads one live channel for the whole process, with turn boundaries visible through
//! `agent_settled` frames; here,
//! [`PiSession::read_until_turn_end`] reads the SAME live channel but returns as soon as one
//! turn's `agent_settled` arrives, exactly like `ClaudeSession::read_until_turn_end` returns on a
//! `"result"` frame. [`open_pi_session`] writes the `get_state` probe and the opening prompt
//! eagerly, before any event sink exists — the same "eager opening write" shape
//! `open_claude_session` uses, not codex's deferred-bootstrap-until-first-`turn()` shape, because
//! nothing here needs to read a response before sending the next command.
//!
//! Two TS mechanisms have no Rust counterpart, and neither is a narrowing:
//! - **`streamingBehavior: 'steer'`.** TS's `sendMessage` sets this when the session's own
//!   `settled` flag is still `false` — i.e. a previous turn is still in flight and the new prompt
//!   should interrupt/steer it rather than queue behind it. That state is unreachable here: every
//!   `turn()`/`send_message()` call already blocks until `agent_settled` before returning, so by
//!   the time a caller can call `send_message` again, the session is always settled. There is
//!   nothing to steer.
//! - **`autoEndAfterFirstTurn` / `AUTO_END_DELAY_MS`.** These belong to TS's one-shot `run()`
//!   wrapper (`startSession(spec, onEvent, {autoEndAfterFirstTurn: true})`), a convenience for
//!   callers that fire a single request and don't want to manage the session's teardown
//!   themselves. Every caller of this trait already manages the session explicitly via its own
//!   `finish()` call, so that convenience has nothing to do here.
//!
//! Token/cost accounting matches `usageValues`' exact formula (`crate::usage::cost_weighted_tokens`,
//! shared with the claude backend) but, unlike claude's `result` frame, pi's wire never reports a
//! separate input/output split at the point usage is read — `SessionReport::input_tokens`/
//! `output_tokens` are `None` here, matching what is actually derivable.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use coducktor_contract::Runner;
use coducktor_core::runs::ask;
use coducktor_core::workflows::run::{
    AgentSession, EventInput, SessionOutcome, SessionReport, TurnMarkerDecision, decide_turn_marker,
};
use serde_json::{Map, Value, json};

use crate::agent_runner::{AgentRunSpec, ContentBlock, reasoning_effort_str};
use crate::child_process::{ChildProcess, NextLine, SpawnConfig};
use crate::claude_runner::{
    DEFAULT_RUN_TIMEOUT_MS, EOF_KILL_GRACE_MS, EOF_TERM_GRACE_MS, KILL_GRACE_MS,
};
use crate::usage::{self, RawUsage};
use crate::wire::{as_nonempty_str, as_record};

/// Where to find the pi binary. Production wiring resolves `program`/`prefix_args` from
/// `DUCK_PI_BIN`/`DUCK_DRY_RUN` are resolved by the session factory;
/// tests point `program` at `node` with `prefix_args: vec![mock_script_path]`.
#[derive(Debug, Clone)]
pub struct PiSpawnConfig {
    pub program: String,
    pub prefix_args: Vec<String>,
    /// Grace period after `finish()` closes stdin before escalating to SIGTERM.
    pub eof_term_grace: Duration,
    /// Grace period after that SIGTERM before escalating to SIGKILL.
    pub eof_kill_grace: Duration,
    /// Grace period after a wall-clock timeout's SIGTERM before escalating to SIGKILL.
    pub kill_grace: Duration,
}

impl Default for PiSpawnConfig {
    fn default() -> Self {
        Self {
            program: "pi".to_owned(),
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
            "`{program}` not found on PATH — install pi and run `pi` once to configure a provider"
        )
    } else {
        error.to_string()
    }
}

/// A live `pi` RPC session. Implements [`AgentSession`].
pub struct PiSession {
    process: ChildProcess,
    session_id: Option<String>,
    /// Whether stdin is still open for this session.
    open: bool,
    /// 0 disables the wall-clock kill switch entirely (interactive sessions).
    timeout_ms: u64,
}

/// Spawn a pi session, probe its state, and send the opening message — mirrors `startSession`'s
/// synchronous `write({type:'get_state'})` + `sendMessage([...images, text])` calls before it
/// returns a session object.
pub fn open_pi_session(
    config: &PiSpawnConfig,
    spec: &AgentRunSpec,
    host_env: &BTreeMap<String, String>,
) -> Result<PiSession, String> {
    let mut args = config.prefix_args.clone();
    args.extend(build_pi_args(spec));
    let process = ChildProcess::spawn(
        &SpawnConfig {
            program: config.program.clone(),
            args,
            eof_term_grace: config.eof_term_grace,
            eof_kill_grace: config.eof_kill_grace,
            kill_grace: config.kill_grace,
        },
        Runner::Pi,
        &spec.cwd,
        &spec.env,
        host_env,
    )
    .map_err(|error| wrap_spawn_error(&error, &config.program))?;

    let timeout_ms = spec.timeout_ms.unwrap_or(DEFAULT_RUN_TIMEOUT_MS);

    let mut session = PiSession {
        process,
        session_id: spec.session_id.clone(),
        open: true,
        timeout_ms,
    };

    session.write_get_state()?;
    let mut opening = spec.images.clone();
    opening.push(ContentBlock::Text {
        text: spec.user_prompt.clone(),
    });
    session.write_prompt(&opening)?;

    Ok(session)
}

/// Build the arguments for a Pi RPC session.
pub fn build_pi_args(spec: &AgentRunSpec) -> Vec<String> {
    let mut args = vec!["--mode".to_owned(), "rpc".to_owned()];
    if let Some(session_id) = &spec.session_id {
        args.push(
            if spec.resume {
                "--session"
            } else {
                "--session-id"
            }
            .to_owned(),
        );
        args.push(session_id.clone());
    }
    if let Some(system_prompt) = &spec.system_prompt {
        args.push("--append-system-prompt".to_owned());
        args.push(system_prompt.clone());
    }
    if let Some(model) = &spec.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = spec.reasoning_effort {
        args.push("--thinking".to_owned());
        args.push(reasoning_effort_str(effort).to_owned());
    }
    let tools = pi_tools(&spec.allowed_tools, &spec.bash_allowlist);
    if !tools.is_empty() {
        args.push("--tools".to_owned());
        args.push(tools.join(","));
    }
    args
}

fn pi_tool_name(tool: &str) -> String {
    match tool {
        "Read" => "read".to_owned(),
        "Bash" => "bash".to_owned(),
        "Edit" => "edit".to_owned(),
        "Write" => "write".to_owned(),
        "Grep" => "grep".to_owned(),
        "Glob" => "find".to_owned(),
        other => other.to_lowercase(),
    }
}

/// Build Pi's tool list. Pi can allow or deny the whole Bash tool but has no command-prefix
/// equivalent — fail closed when a workflow requests that narrower mode.
fn pi_tools(tools: &[String], bash_allowlist: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for tool in tools {
        if tool == "Bash" && !bash_allowlist.is_empty() {
            continue;
        }
        let mapped = pi_tool_name(tool);
        if seen.insert(mapped.clone()) {
            out.push(mapped);
        }
    }
    out
}

/// Convert prompt content to Pi's text and image payloads.
fn to_pi_prompt(content: &[ContentBlock]) -> (String, Vec<Value>) {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::Image { source } => images.push(json!({
                "type": "image",
                "data": source.data,
                "mimeType": source.media_type,
            })),
        }
    }
    (text_parts.join("\n"), images)
}

/// Read usage and cost values when the payload is a record.
fn usage_values(value: Option<&Value>) -> Option<(f64, f64)> {
    let record = as_record(value?)?;
    let raw = RawUsage {
        input_tokens: record.get("input").and_then(Value::as_f64),
        output_tokens: record.get("output").and_then(Value::as_f64),
        cache_creation_input_tokens: record.get("cacheWrite").and_then(Value::as_f64),
        cache_read_input_tokens: record.get("cacheRead").and_then(Value::as_f64),
    };
    let weighted = usage::cost_weighted_tokens(Some(&raw));
    let cost = record
        .get("cost")
        .and_then(as_record)
        .and_then(|cost| cost.get("total"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Some((weighted, cost))
}

/// Extract text from a Pi content payload.
fn content_text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let texts: Vec<String> = parts
                .iter()
                .filter_map(|part| {
                    let record = as_record(part)?;
                    if record.get("type").and_then(Value::as_str) == Some("text") {
                        record
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

struct PiImage {
    media_type: String,
    data: String,
}

/// Extract Pi tool-result images. Pi's image shape (`data`/`mimeType` directly on the
/// part) is distinct from claude's Anthropic-shaped `source.media_type`/`source.data`.
fn tool_result_images(value: Option<&Value>) -> Vec<PiImage> {
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
            Some(PiImage {
                media_type: media_type.to_owned(),
                data: data.to_owned(),
            })
        })
        .collect()
}

/// Extract a useful message from a failed Pi RPC record.
fn rpc_error_message(record: &Map<String, Value>) -> String {
    if let Some(error) = record.get("error").and_then(as_record)
        && let Some(message) = as_nonempty_str(error.get("message"))
    {
        return message.to_owned();
    }
    if let Some(message) = as_nonempty_str(record.get("message")) {
        return message.to_owned();
    }
    let command = record
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("pi RPC command {command} failed")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let head: String = text.chars().take(max).collect();
        format!("{head}…")
    } else {
        text.to_owned()
    }
}

impl PiSession {
    fn write_get_state(&mut self) -> Result<(), String> {
        self.process
            .write_line(&json!({"id": "coducktor-state", "type": "get_state"}).to_string())
    }

    fn write_prompt(&mut self, content: &[ContentBlock]) -> Result<(), String> {
        let (message, images) = to_pi_prompt(content);
        let mut command = Map::new();
        command.insert("type".to_owned(), Value::String("prompt".to_owned()));
        command.insert("message".to_owned(), Value::String(message));
        if !images.is_empty() {
            command.insert("images".to_owned(), Value::Array(images));
        }
        let line =
            serde_json::to_string(&Value::Object(command)).map_err(|error| error.to_string())?;
        self.process.write_line(&line)
    }

    /// The wall-clock kill switch a live turn's read loop arms when `timeout_ms` elapses.
    fn escalate_after_timeout(&mut self) -> String {
        self.open = false;
        self.process.escalate_after_timeout();
        let minutes = (self.timeout_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
        format!("pi CLI timed out after {minutes}m and was killed")
    }

    fn finalize_turn(
        &self,
        text_chunks: Vec<String>,
        tokens_used: f64,
        cost_usd: Option<f64>,
    ) -> Result<SessionOutcome, String> {
        // Unlike claude/codex's whole-block text events, pi emits raw incremental deltas — the
        // TS source joins them with `''`, not `'\n'`.
        let turn_text = text_chunks.join("").trim().to_owned();
        let valid_ask = ask::parse_ask_marker(&turn_text).is_some();
        let decision = decide_turn_marker(&turn_text, self.open, valid_ask);
        let report = SessionReport {
            session_id: self.session_id.clone(),
            tokens_used,
            input_tokens: None,
            output_tokens: None,
            cost_usd,
            turn_text,
            decision: Some(decision),
            plan_entries: None,
        };
        Ok(if decision == TurnMarkerDecision::Done {
            SessionOutcome::Completed(report)
        } else {
            SessionOutcome::Waiting(report)
        })
    }

    fn read_until_turn_end(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if !self.open {
            return Err("session is closed".to_owned());
        }
        let deadline =
            (self.timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(self.timeout_ms));
        let mut text_chunks: Vec<String> = Vec::new();
        let mut tokens_used = 0.0_f64;
        let mut cost_usd: Option<f64> = None;

        loop {
            let line = match self.process.next_line(deadline) {
                Ok(NextLine::Line(line)) => line,
                Ok(NextLine::Closed) => break,
                Err(_timed_out) => {
                    let message = self.escalate_after_timeout();
                    on_event(EventInput::new("error").field("message", message.clone()))
                        .map_err(|error| error.to_string())?;
                    return Err(message);
                }
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(_) => {
                    on_event(EventInput::new("note").field(
                        "message",
                        format!("pi: skipped unparseable RPC line: {}", truncate(line, 200)),
                    ))
                    .map_err(|error| error.to_string())?;
                    continue;
                }
            };
            let Some(record) = as_record(&value) else {
                continue;
            };
            let Some(kind) = record.get("type").and_then(Value::as_str) else {
                continue;
            };

            match kind {
                "response" => {
                    if record.get("command").and_then(Value::as_str) == Some("get_state")
                        && record.get("success").and_then(Value::as_bool) == Some(true)
                        && let Some(data) = record.get("data").and_then(as_record)
                    {
                        if let Some(discovered) = as_nonempty_str(data.get("sessionId"))
                            && Some(discovered) != self.session_id.as_deref()
                        {
                            self.session_id = Some(discovered.to_owned());
                            on_event(EventInput::new("session").field("sessionId", discovered))
                                .map_err(|error| error.to_string())?;
                        }
                    } else if record.get("success").and_then(Value::as_bool) == Some(false) {
                        on_event(
                            EventInput::new("error").field("message", rpc_error_message(record)),
                        )
                        .map_err(|error| error.to_string())?;
                    }
                }
                "message_update" => {
                    if let Some(update) = record.get("assistantMessageEvent").and_then(as_record)
                        && update.get("type").and_then(Value::as_str) == Some("text_delta")
                        && let Some(delta) = update.get("delta").and_then(Value::as_str)
                    {
                        text_chunks.push(delta.to_owned());
                        on_event(EventInput::new("text").field("text", delta))
                            .map_err(|error| error.to_string())?;
                    }
                }
                "message_end" => {
                    let is_assistant = record
                        .get("message")
                        .and_then(as_record)
                        .and_then(|message| message.get("role"))
                        .and_then(Value::as_str)
                        == Some("assistant");
                    if is_assistant
                        && let Some((weighted, cost)) =
                            usage_values(record.get("message").and_then(|m| m.get("usage")))
                    {
                        tokens_used += weighted;
                        on_event(EventInput::new("token-usage").field("tokensUsed", tokens_used))
                            .map_err(|error| error.to_string())?;
                        if cost > 0.0 {
                            cost_usd = Some(cost);
                            on_event(EventInput::new("cost").field("usd", cost))
                                .map_err(|error| error.to_string())?;
                        }
                    }
                }
                "tool_execution_start" => {
                    if let (Some(id), Some(name)) = (
                        as_nonempty_str(record.get("toolCallId")),
                        as_nonempty_str(record.get("toolName")),
                    ) {
                        on_event(
                            EventInput::new("tool-call")
                                .field("id", id)
                                .field("tool", name)
                                .field("input", record.get("args").cloned().unwrap_or(Value::Null)),
                        )
                        .map_err(|error| error.to_string())?;
                    }
                }
                "tool_execution_end" => {
                    if let Some(id) = as_nonempty_str(record.get("toolCallId")) {
                        let content = record
                            .get("result")
                            .and_then(as_record)
                            .and_then(|result| result.get("content"));
                        on_event(
                            EventInput::new("tool-result")
                                .field("toolCallId", id)
                                .field("result", content_text(content).unwrap_or_default())
                                .field(
                                    "isError",
                                    record
                                        .get("isError")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                ),
                        )
                        .map_err(|error| error.to_string())?;
                        for image in tool_result_images(content) {
                            on_event(
                                EventInput::new("image")
                                    .field("mediaType", image.media_type)
                                    .field("data", image.data),
                            )
                            .map_err(|error| error.to_string())?;
                        }
                    }
                }
                "agent_settled" => {
                    // `settled` only matters for the post-loop "channel closed early" note below;
                    // this branch returns before that check runs.
                    on_event(EventInput::new("turn-end")).map_err(|error| error.to_string())?;
                    if tokens_used == 0.0 {
                        on_event(
                            EventInput::new("note")
                                .field("message", "token usage not reported by pi CLI"),
                        )
                        .map_err(|error| error.to_string())?;
                    }
                    return self.finalize_turn(text_chunks, tokens_used, cost_usd);
                }
                "extension_error" => {
                    let message = as_nonempty_str(record.get("error"))
                        .or_else(|| as_nonempty_str(record.get("message")))
                        .unwrap_or("pi extension error");
                    on_event(EventInput::new("note").field("message", message))
                        .map_err(|error| error.to_string())?;
                }
                _ => {}
            }
        }

        // stdout closed without agent_settled — the process exited (or crashed) mid-turn.
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
            let message = format!("pi CLI exited with code {code}{detail}");
            on_event(EventInput::new("error").field("message", message.clone()))
                .map_err(|error| error.to_string())?;
            return Err(message);
        }
        // Reaching here means the loop above never saw `agent_settled` (that branch returns
        // directly) — the channel simply closed mid-turn.
        on_event(
            EventInput::new("note").field("message", "pi RPC session ended before agent_settled"),
        )
        .map_err(|error| error.to_string())?;
        if tokens_used == 0.0 {
            on_event(
                EventInput::new("note").field("message", "token usage not reported by pi CLI"),
            )
            .map_err(|error| error.to_string())?;
        }
        self.finalize_turn(text_chunks, tokens_used, cost_usd)
    }
}

impl AgentSession for PiSession {
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        self.read_until_turn_end(on_event)
    }

    fn send_message(
        &mut self,
        prompt: &str,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if !self.open {
            return Err("session does not accept follow-up messages".to_owned());
        }
        self.write_prompt(&[ContentBlock::Text {
            text: prompt.to_owned(),
        }])?;
        self.read_until_turn_end(on_event)
    }

    fn finish(
        &mut self,
        _on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if self.open {
            self.open = false;
            self.process.close_stdin(); // delivers EOF to the child
            self.process.escalate_after_eof();
        }
        self.process.wait_for_exit();
        Ok(SessionOutcome::Completed(SessionReport::default()))
    }

    fn cancel(&mut self) {
        self.open = false;
        // Best-effort, matching TS's `interrupt()`: an abort command, then a hard stop.
        let _ = self
            .process
            .write_line(&json!({"type": "abort"}).to_string());
        if !self.process.has_exited() {
            self.process.signal_term();
        }
    }

    fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::ConcreteReasoningEffort;
    use std::path::PathBuf;

    fn mock_script() -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../fixtures/scripts/mock-pi-rpc.mjs");
        path.canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn node_config() -> PiSpawnConfig {
        PiSpawnConfig {
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

    // ---- pure argv-building tests ---------------------------------------------------------

    #[test]
    fn build_pi_args_passes_the_selected_thinking_level() {
        let spec = AgentRunSpec {
            user_prompt: "task".to_owned(),
            reasoning_effort: Some(ConcreteReasoningEffort::XHigh),
            ..Default::default()
        };
        let args = build_pi_args(&spec);
        assert!(args.iter().any(|arg| arg == "--thinking"));
        assert!(args.iter().any(|arg| arg == "xhigh"));
    }

    #[test]
    fn build_pi_args_uses_rpc_mode_exact_session_model_and_tool_names() {
        let spec = AgentRunSpec {
            user_prompt: "task".to_owned(),
            session_id: Some("session-1".to_owned()),
            resume: true,
            model: Some("openai/gpt-5.1".to_owned()),
            system_prompt: Some("Keep changes focused.".to_owned()),
            allowed_tools: vec![
                "Read".to_owned(),
                "Bash".to_owned(),
                "Edit".to_owned(),
                "Write".to_owned(),
                "Grep".to_owned(),
                "Glob".to_owned(),
            ],
            ..Default::default()
        };
        assert_eq!(
            build_pi_args(&spec),
            vec![
                "--mode",
                "rpc",
                "--session",
                "session-1",
                "--append-system-prompt",
                "Keep changes focused.",
                "--model",
                "openai/gpt-5.1",
                "--tools",
                "read,bash,edit,write,grep,find",
            ]
        );
    }

    #[test]
    fn build_pi_args_creates_a_new_exact_session_id_instead_of_resuming() {
        let spec = AgentRunSpec {
            user_prompt: "task".to_owned(),
            session_id: Some("session-1".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            build_pi_args(&spec),
            vec!["--mode", "rpc", "--session-id", "session-1"]
        );
    }

    #[test]
    fn build_pi_args_fails_bash_closed_when_a_command_prefix_allowlist_cannot_be_represented() {
        let spec = AgentRunSpec {
            user_prompt: "task".to_owned(),
            allowed_tools: vec!["Read".to_owned(), "Bash".to_owned()],
            bash_allowlist: vec!["npm test".to_owned()],
            ..Default::default()
        };
        assert_eq!(
            build_pi_args(&spec),
            vec!["--mode", "rpc", "--tools", "read"]
        );
    }

    // ---- real-subprocess tests against the bundled dry-run mock ----------------------------------

    fn run_turn(session: &mut PiSession) -> (Result<SessionOutcome, String>, Vec<EventInput>) {
        let mut events = Vec::new();
        let result = session.turn(&mut |event| {
            events.push(event);
            Ok(())
        });
        (result, events)
    }

    #[test]
    fn a_first_turn_against_the_mock_streams_text_tool_and_usage_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "investigate the login redirect bug");
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
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

        // No DUCK:DONE in the prompt — the turn parks as Waiting rather than completing.
        match outcome {
            SessionOutcome::Waiting(report) => {
                assert!(report.turn_text.contains("Investigating"));
                assert!(report.tokens_used > 0.0);
            }
            other => panic!("expected Waiting, got {other:?}"),
        }

        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_done_marked_turn_completes_the_step() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "please finish this mock:done");
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, _events) = run_turn(&mut session);
        match outcome.unwrap() {
            SessionOutcome::Completed(report) => {
                assert!(report.turn_text.contains("Investigating"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_follow_up_message_reaches_a_second_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "first turn, no marker");
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        run_turn(&mut session)
            .0
            .expect("first turn should complete");

        let outcome = session
            .send_message("second turn mock:done", &mut |_| Ok(()))
            .unwrap();
        match outcome {
            SessionOutcome::Completed(report) => {
                assert!(report.turn_text.contains("second turn"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn finish_closes_a_cooperative_process_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "hello mock:done");
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        run_turn(&mut session)
            .0
            .expect("first turn should complete");
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn finish_escalates_through_sigterm_against_a_process_that_ignores_eof() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = node_config();
        config.eof_term_grace = Duration::from_millis(80);
        config.eof_kill_grace = Duration::from_millis(200);
        let mut run_spec = spec_for(dir.path(), "anything");
        run_spec
            .env
            .insert("MOCK_PI_IGNORE_EOF".to_owned(), "1".to_owned());
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_wall_clock_timeout_kills_the_process_and_fails_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let mut run_spec = spec_for(dir.path(), "please wait mock:slow");
        run_spec.timeout_ms = Some(300);
        let mut session = open_pi_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        let error = outcome.expect_err("a hung turn should fail rather than park forever");
        assert!(error.contains("timed out"), "unexpected message: {error}");
        assert!(events.iter().any(|event| event.event_type == "error"));
    }

    #[test]
    fn spawn_failure_reports_a_friendly_missing_binary_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = PiSpawnConfig {
            program: "coducktor-test-nonexistent-binary-xyz".to_owned(),
            ..Default::default()
        };
        let run_spec = spec_for(dir.path(), "hi");
        let error = match open_pi_session(&config, &run_spec, &BTreeMap::new()) {
            Err(error) => error,
            Ok(_) => panic!("expected spawn to fail"),
        };
        assert!(
            error.contains("not found on PATH"),
            "unexpected message: {error}"
        );
    }
}
