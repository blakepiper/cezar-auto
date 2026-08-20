//! `AgentSession` over the Claude Code CLI in headless stream-json mode. Auth is the host's
//! logged-in Pro/Max
//! subscription (no API key needed). Sandboxing is `--allowedTools` (default-deny for anything
//! not listed) + running inside the repo `cwd`; `Bash` is narrowed to `Bash(<prefix>:*)`
//! patterns only when `bash_allowlist` is set.
//!
//! Each call to `turn()` or `send_message()` consumes one Claude result frame. Shared process
//! plumbing handles stdout, stderr, EOF, and timeout escalation; this module maps Claude's
//! stream-json frames into the normalized event protocol.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use coducktor_contract::Runner;
use coducktor_core::workflows::run::{
    AgentSession, EventInput, PromptImage, SessionOutcome, SessionReport, TurnMarkerDecision,
    decide_turn_marker,
};
use serde_json::{Map, Value};

use crate::agent_runner::{AgentRunSpec, ContentBlock, prompt_content, reasoning_effort_str};
use crate::child_process::{ChildProcess, NextLine, SpawnConfig};
use crate::claude::{stringify_tool_result_content, tool_result_image_blocks};
use crate::usage::{self, RawUsage};

/// Default wall-clock cap for a single run before SIGTERM -> SIGKILL. Interactive sessions pass
/// `timeout_ms: Some(0)` to disable it entirely.
pub const DEFAULT_RUN_TIMEOUT_MS: u64 = 30 * 60_000;
/// Grace period between SIGTERM and SIGKILL when a wall-clock timeout fires.
pub const KILL_GRACE_MS: u64 = 10_000;
/// After `finish()` closes stdin: claude in stream-json mode can ignore EOF and hang — escalate
/// SIGTERM, then SIGKILL.
pub const EOF_TERM_GRACE_MS: u64 = 8_000;
pub const EOF_KILL_GRACE_MS: u64 = 4_000;

/// Build the headless argv. `--input-format stream-json` reads user messages from stdin;
/// `--output-format stream-json --verbose` gives per-event NDJSON; `--permission-mode dontAsk`
/// keeps headless runs non-interactive: tools in `--allowedTools` proceed and everything else is
/// denied instead of prompting. `DUCK_APPROVAL_GATE=1` opts back into Claude's approval UI.
pub fn build_claude_args(spec: &AgentRunSpec, env: &BTreeMap<String, String>) -> Vec<String> {
    let mut args = vec![
        "--input-format".to_owned(),
        "stream-json".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--verbose".to_owned(),
        "--forward-subagent-text".to_owned(),
        "--permission-mode".to_owned(),
        if env.get("DUCK_APPROVAL_GATE").map(String::as_str) == Some("1") {
            "acceptEdits".to_owned()
        } else {
            "dontAsk".to_owned()
        },
    ];
    if let Some(system_prompt) = &spec.system_prompt {
        args.push("--append-system-prompt".to_owned());
        args.push(system_prompt.clone());
    }
    // Pin the session so the user can `claude --resume <sessionId>` in the repo to take over
    // interactively after a run. With `resume` we reopen the existing on-disk conversation
    // instead.
    if let Some(session_id) = &spec.session_id {
        args.push(
            if spec.resume {
                "--resume"
            } else {
                "--session-id"
            }
            .to_owned(),
        );
        args.push(session_id.clone());
    }
    let allowed = build_allowed_tools(&spec.allowed_tools, &spec.bash_allowlist);
    if !allowed.is_empty() {
        args.push("--allowedTools".to_owned());
        args.push(allowed.join(","));
    }
    if let Some(model) = &spec.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = spec.reasoning_effort {
        args.push("--effort".to_owned());
        args.push(reasoning_effort_str(effort).to_owned());
    }
    for dir in &spec.additional_directories {
        args.push("--add-dir".to_owned());
        args.push(dir.clone());
    }
    args
}

/// Map `allowed_tools` onto claude's `--allowedTools` syntax. `Bash` with a `bash_allowlist`
/// becomes one `Bash(<prefix>:*)` entry per allowed prefix; `Bash` with no allowlist stays plain
/// `Bash`.
pub fn build_allowed_tools(allowed_tools: &[String], bash_allowlist: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for tool in allowed_tools {
        if tool == "Bash" && !bash_allowlist.is_empty() {
            for prefix in bash_allowlist {
                let prefix = prefix.trim();
                if !prefix.is_empty() {
                    out.push(format!("Bash({prefix}:*)"));
                }
            }
        } else {
            out.push(tool.clone());
        }
    }
    out
}

/// Where to find the claude binary. Production wiring resolves `program`/`prefix_args` from
/// `DUCK_CLAUDE_BIN`/`DUCK_DRY_RUN` (resolved by the session factory,
/// — it is the same "injected by the integration layer" seam `SessionFactory` already is); tests
/// point `program` at `node` with `prefix_args: vec![mock_script_path]`.
#[derive(Debug, Clone)]
pub struct ClaudeSpawnConfig {
    pub program: String,
    pub prefix_args: Vec<String>,
    /// Grace period after `finish()` closes stdin before escalating to SIGTERM.
    pub eof_term_grace: Duration,
    /// Grace period after that SIGTERM before escalating to SIGKILL.
    pub eof_kill_grace: Duration,
    /// Grace period after a wall-clock timeout's SIGTERM before escalating to SIGKILL.
    pub kill_grace: Duration,
}

impl Default for ClaudeSpawnConfig {
    fn default() -> Self {
        Self {
            program: "claude".to_owned(),
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
            "`{program}` not found on PATH — install Claude Code (https://claude.com/claude-code) and run `claude` once to log in"
        )
    } else {
        error.to_string()
    }
}

/// A live `claude` CLI session. Implements [`AgentSession`].
pub struct ClaudeSession {
    process: ChildProcess,
    session_id: Option<String>,
    /// Whether stdin is still open for this session.
    open: bool,
    /// 0 disables the wall-clock kill switch entirely (interactive sessions).
    timeout_ms: u64,
}

/// Spawn a claude session and send the opening message — mirrors `startSession`'s synchronous
/// `sendMessage([...images, text])` call before it returns a session object. Pasted task
/// screenshots (`spec.images`) ride along as leading content blocks.
pub fn open_claude_session(
    config: &ClaudeSpawnConfig,
    spec: &AgentRunSpec,
    host_env: &BTreeMap<String, String>,
) -> Result<ClaudeSession, String> {
    let mut args = config.prefix_args.clone();
    args.extend(build_claude_args(spec, host_env));
    let mut process = ChildProcess::spawn(
        &SpawnConfig {
            program: config.program.clone(),
            args,
            eof_term_grace: config.eof_term_grace,
            eof_kill_grace: config.eof_kill_grace,
            kill_grace: config.kill_grace,
        },
        Runner::Claude,
        &spec.cwd,
        &spec.env,
        host_env,
    )
    .map_err(|error| wrap_spawn_error(&error, &config.program))?;
    process.set_cancellation(spec.cancellation.clone());

    let timeout_ms = spec.timeout_ms.unwrap_or(DEFAULT_RUN_TIMEOUT_MS);

    let mut session = ClaudeSession {
        process,
        session_id: spec.session_id.clone(),
        open: true,
        timeout_ms,
    };

    let mut opening = spec.images.clone();
    opening.push(ContentBlock::Text {
        text: spec.user_prompt.clone(),
    });
    session.write_message(&opening)?;

    Ok(session)
}

struct ClaudeMessageResult {
    events: Vec<EventInput>,
    usage_delta: f64,
    error: Option<String>,
}

fn parse_raw_usage(value: Option<&Value>) -> Option<RawUsage> {
    let obj = value?.as_object()?;
    Some(RawUsage {
        input_tokens: obj.get("input_tokens").and_then(Value::as_f64),
        output_tokens: obj.get("output_tokens").and_then(Value::as_f64),
        cache_creation_input_tokens: obj
            .get("cache_creation_input_tokens")
            .and_then(Value::as_f64),
        cache_read_input_tokens: obj.get("cache_read_input_tokens").and_then(Value::as_f64),
    })
}

/// Map a Claude message into normalized events. Every accessor is `Option`-based rather than an
/// indexing operation that could panic on an unexpected shape: agent processes are external and
/// their wire formats evolve, so malformed frames are ignored rather than taking down the run.
fn handle_claude_message(msg: &Value, text_chunks: &mut Vec<String>) -> ClaudeMessageResult {
    let mut events = Vec::new();
    match msg.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if let Some(content) = msg.pointer("/message/content").and_then(Value::as_array) {
                for block in content {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                text_chunks.push(text.to_owned());
                                events.push(EventInput::new("text").field("text", text));
                            }
                        }
                        Some("thinking") | Some("reasoning") => {
                            if let Some(text) = block
                                .get("thinking")
                                .or_else(|| block.get("text"))
                                .and_then(Value::as_str)
                                .filter(|text| !text.is_empty())
                            {
                                events.push(EventInput::new("reasoning").field("text", text));
                            }
                        }
                        Some("tool_use") => {
                            if let (Some(id), Some(name)) = (
                                block.get("id").and_then(Value::as_str),
                                block.get("name").and_then(Value::as_str),
                            ) {
                                events.push(
                                    EventInput::new("tool-call")
                                        .field("id", id)
                                        .field("tool", name)
                                        .field(
                                            "input",
                                            block.get("input").cloned().unwrap_or(Value::Null),
                                        ),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            ClaudeMessageResult {
                events,
                usage_delta: 0.0,
                error: None,
            }
        }
        Some("user") => {
            if let Some(content) = msg.pointer("/message/content").and_then(Value::as_array) {
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    events.push(
                        EventInput::new("tool-result")
                            .field("toolCallId", tool_use_id)
                            .field(
                                "result",
                                stringify_tool_result_content(block.get("content")),
                            )
                            .field(
                                "isError",
                                block
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            ),
                    );
                    for image in tool_result_image_blocks(block.get("content")) {
                        events.push(
                            EventInput::new("image")
                                .field("mediaType", image.media_type)
                                .field("data", image.data),
                        );
                    }
                }
            }
            ClaudeMessageResult {
                events,
                usage_delta: 0.0,
                error: None,
            }
        }
        Some("result") => {
            let is_error = msg.get("is_error").and_then(Value::as_bool) == Some(true);
            if let Some(result_text) = msg.get("result").and_then(Value::as_str)
                && text_chunks.is_empty()
                && !is_error
            {
                text_chunks.push(result_text.to_owned());
                events.push(EventInput::new("text").field("text", result_text));
            }
            let error = if is_error {
                let message = msg
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| match msg.get("subtype").and_then(Value::as_str) {
                        Some(subtype) => format!("claude reported result error ({subtype})"),
                        None => "claude reported result error".to_owned(),
                    });
                events.push(EventInput::new("error").field("message", message.clone()));
                Some(message)
            } else {
                None
            };
            let usage_delta =
                usage::cost_weighted_tokens(parse_raw_usage(msg.get("usage")).as_ref());
            ClaudeMessageResult {
                events,
                usage_delta,
                error,
            }
        }
        // system/init and anything else: nothing actionable.
        _ => ClaudeMessageResult {
            events,
            usage_delta: 0.0,
            error: None,
        },
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let head: String = text.chars().take(max).collect();
        format!("{head}…")
    } else {
        text.to_owned()
    }
}

impl ClaudeSession {
    fn write_message(&mut self, content: &[ContentBlock]) -> Result<(), String> {
        let mut message = Map::new();
        message.insert("role".to_owned(), Value::String("user".to_owned()));
        message.insert(
            "content".to_owned(),
            serde_json::to_value(content).map_err(|error| error.to_string())?,
        );
        let mut envelope = Map::new();
        envelope.insert("type".to_owned(), Value::String("user".to_owned()));
        envelope.insert("message".to_owned(), Value::Object(message));
        // Omitted entirely (not `null`) when unset, matching `JSON.stringify` dropping an
        // `undefined` field — the real CLI's session-id inference depends on the key being
        // absent, not present-and-null.
        if let Some(session_id) = &self.session_id {
            envelope.insert("session_id".to_owned(), Value::String(session_id.clone()));
        }
        let line =
            serde_json::to_string(&Value::Object(envelope)).map_err(|error| error.to_string())?;
        self.process.write_line(&line)
    }

    /// The wall-clock kill switch a live turn's read loop arms when `timeout_ms` elapses.
    fn escalate_after_timeout(&mut self) -> String {
        self.open = false;
        self.process.escalate_after_timeout();
        let minutes = (self.timeout_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
        format!("claude CLI timed out after {minutes}m and was killed")
    }

    fn finalize_turn(
        &self,
        text_chunks: Vec<String>,
        tokens_used: f64,
        last_usage: Option<RawUsage>,
        cost_usd: Option<f64>,
        error: Option<String>,
    ) -> Result<SessionOutcome, String> {
        let turn_text = text_chunks.join("\n").trim().to_owned();
        let valid_ask = coducktor_core::runs::ask::parse_ask_marker(&turn_text).is_some();
        let decision = decide_turn_marker(&turn_text, self.open, valid_ask);
        let report = SessionReport {
            session_id: self.session_id.clone(),
            tokens_used,
            input_tokens: last_usage.and_then(|usage| usage.input_tokens),
            output_tokens: last_usage.and_then(|usage| usage.output_tokens),
            cost_usd,
            turn_text,
            decision: Some(decision),
            plan_entries: None,
        };
        Ok(if let Some(message) = error {
            SessionOutcome::Failed { message, report }
        } else if decision == TurnMarkerDecision::Done {
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
        let mut saw_usage = false;
        let mut tokens_used = 0.0_f64;
        let mut last_usage: Option<RawUsage> = None;
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
            let msg: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(_) => {
                    on_event(EventInput::new("note").field(
                        "message",
                        format!(
                            "claude: skipped unparseable stream line: {}",
                            truncate(line, 200)
                        ),
                    ))
                    .map_err(|error| error.to_string())?;
                    continue;
                }
            };
            let mapped = handle_claude_message(&msg, &mut text_chunks);
            for event in mapped.events {
                on_event(event).map_err(|error| error.to_string())?;
            }
            if mapped.usage_delta > 0.0 {
                saw_usage = true;
                tokens_used += mapped.usage_delta;
                on_event(EventInput::new("token-usage").field("tokensUsed", tokens_used))
                    .map_err(|error| error.to_string())?;
            }
            if msg.get("type").and_then(Value::as_str) == Some("result") {
                if let Some(cost) = msg.get("total_cost_usd").and_then(Value::as_f64)
                    && cost > 0.0
                {
                    cost_usd = Some(cost);
                    on_event(EventInput::new("cost").field("usd", cost))
                        .map_err(|error| error.to_string())?;
                }
                last_usage = parse_raw_usage(msg.get("usage"));
                on_event(EventInput::new("turn-end")).map_err(|error| error.to_string())?;
                if !saw_usage {
                    on_event(
                        EventInput::new("note")
                            .field("message", "token usage not reported by claude CLI"),
                    )
                    .map_err(|error| error.to_string())?;
                }
                return self.finalize_turn(
                    text_chunks,
                    tokens_used,
                    last_usage,
                    cost_usd,
                    mapped.error,
                );
            }
        }

        // stdout closed without a `result` frame — the process exited (or crashed) mid-turn.
        // No signal-termination bookkeeping to consult here: see the module doc's note on why
        // `terminatedByCoducktor` has no Rust counterpart in this turn-scoped session.
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
            let message = format!("claude CLI exited with code {code}{detail}");
            on_event(EventInput::new("error").field("message", message.clone()))
                .map_err(|error| error.to_string())?;
            return Err(message);
        }
        if !saw_usage {
            on_event(
                EventInput::new("note").field("message", "token usage not reported by claude CLI"),
            )
            .map_err(|error| error.to_string())?;
        }
        self.finalize_turn(text_chunks, tokens_used, last_usage, cost_usd, None)
    }
}

impl AgentSession for ClaudeSession {
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        self.read_until_turn_end(on_event)
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
        self.write_message(&prompt_content(prompt, images))?;
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

    fn mock_script(name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../fixtures/scripts");
        path.push(name);
        path.canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn fixture_script(name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../fixtures/claude");
        path.push(name);
        path.canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn node_config(script: String) -> ClaudeSpawnConfig {
        ClaudeSpawnConfig {
            program: crate::test_node_program(),
            prefix_args: vec![script],
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
    fn build_claude_args_emits_append_system_prompt_when_set() {
        let spec = AgentRunSpec {
            user_prompt: "do it".to_owned(),
            system_prompt: Some("Extra rules.\n\n---\n\nContract.".to_owned()),
            ..Default::default()
        };
        let args = build_claude_args(&spec, &BTreeMap::new());
        let idx = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .expect("flag present");
        assert_eq!(args[idx + 1], "Extra rules.\n\n---\n\nContract.");
    }

    #[test]
    fn build_claude_args_omits_the_flag_when_no_system_prompt() {
        let spec = AgentRunSpec {
            user_prompt: "do it".to_owned(),
            ..Default::default()
        };
        let args = build_claude_args(&spec, &BTreeMap::new());
        assert!(!args.iter().any(|arg| arg == "--append-system-prompt"));
    }

    #[test]
    fn build_claude_args_forwards_nested_agent_text() {
        let spec = AgentRunSpec {
            user_prompt: "delegate it".to_owned(),
            ..Default::default()
        };
        let args = build_claude_args(&spec, &BTreeMap::new());
        assert!(args.iter().any(|arg| arg == "--forward-subagent-text"));
    }

    #[test]
    fn build_claude_args_denies_unapproved_tools_by_default() {
        let spec = AgentRunSpec {
            user_prompt: "do it".to_owned(),
            ..Default::default()
        };
        let args = build_claude_args(&spec, &BTreeMap::new());
        let idx = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .unwrap();
        assert_eq!(args[idx + 1], "dontAsk");
    }

    #[test]
    fn build_claude_args_enables_approval_gate_when_requested() {
        let spec = AgentRunSpec {
            user_prompt: "do it".to_owned(),
            ..Default::default()
        };
        let mut env = BTreeMap::new();
        env.insert("DUCK_APPROVAL_GATE".to_owned(), "1".to_owned());
        let args = build_claude_args(&spec, &env);
        let idx = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .unwrap();
        assert_eq!(args[idx + 1], "acceptEdits");
    }

    #[test]
    fn build_claude_args_passes_the_selected_reasoning_effort() {
        let spec = AgentRunSpec {
            user_prompt: "do it".to_owned(),
            reasoning_effort: Some(ConcreteReasoningEffort::High),
            ..Default::default()
        };
        let args = build_claude_args(&spec, &BTreeMap::new());
        let idx = args.iter().position(|arg| arg == "--effort").unwrap();
        assert_eq!(args[idx + 1], "high");
    }

    #[test]
    fn build_allowed_tools_expands_bash_allowlist_into_scoped_entries() {
        let allowed = build_allowed_tools(
            &["Read".to_owned(), "Bash".to_owned()],
            &["npm test".to_owned(), "git status".to_owned()],
        );
        assert_eq!(
            allowed,
            vec!["Read", "Bash(npm test:*)", "Bash(git status:*)"]
        );
    }

    #[test]
    fn build_allowed_tools_keeps_bash_unrestricted_without_an_allowlist() {
        let allowed = build_allowed_tools(&["Bash".to_owned()], &[]);
        assert_eq!(allowed, vec!["Bash"]);
    }

    // ---- real-subprocess tests against the bundled dry-run mock ----------------------------------

    fn run_turn(session: &mut ClaudeSession) -> (Result<SessionOutcome, String>, Vec<EventInput>) {
        let mut events = Vec::new();
        let result = session.turn(&mut |event| {
            events.push(event);
            Ok(())
        });
        (result, events)
    }

    #[test]
    fn a_first_turn_against_the_mock_streams_text_tool_and_image_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let run_spec = spec_for(dir.path(), "please look into this");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        let outcome = outcome.expect("first turn should complete");

        let event_types: Vec<&str> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        assert!(event_types.contains(&"text"));
        assert!(event_types.contains(&"tool-call"));
        assert!(event_types.contains(&"tool-result"));
        assert!(event_types.contains(&"image"));
        assert!(event_types.contains(&"token-usage"));
        assert!(event_types.contains(&"turn-end"));

        // No DUCK:DONE in the prompt — the mock's first-turn reply just asks a follow-up
        // question, so the turn parks as Waiting rather than completing the step.
        match outcome {
            SessionOutcome::Waiting(report) => {
                assert!(report.turn_text.contains("Done with the first pass"));
                assert!(report.tokens_used > 0.0);
            }
            other => panic!("expected Waiting, got {other:?}"),
        }

        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_done_marked_turn_completes_the_step() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let run_spec = spec_for(dir.path(), "please finish this mock:done");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, _events) = run_turn(&mut session);
        match outcome.unwrap() {
            SessionOutcome::Completed(report) => {
                assert!(report.turn_text.contains("Done with the first pass"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_usage_limit_result_is_a_failed_turn_not_a_waiting_question() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let run_spec = spec_for(dir.path(), "test quota handling mock:limit");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);

        match outcome.unwrap() {
            SessionOutcome::Failed { message, .. } => {
                assert!(message.starts_with("Claude AI usage limit reached|"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(events.iter().any(|event| event.event_type == "error"));
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn a_follow_up_message_reaches_a_second_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let run_spec = spec_for(dir.path(), "first turn, no marker");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        run_turn(&mut session)
            .0
            .expect("first turn should complete");

        let outcome = session
            .send_message("second turn mock:done", &[], &mut |_| Ok(()))
            .unwrap();
        match outcome {
            SessionOutcome::Completed(report) => {
                assert!(report.turn_text.to_lowercase().contains("follow-up"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn finish_closes_a_cooperative_process_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let run_spec = spec_for(dir.path(), "hello mock:done");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        run_turn(&mut session)
            .0
            .expect("first turn should complete");
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        // The mock exits cleanly on stdin EOF — this must not need the EOF watchdog's grace
        // periods at all.
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn finish_escalates_through_sigterm_against_a_process_that_ignores_eof() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = node_config(fixture_script("stub-ignores-eof-exits-143.mjs"));
        config.eof_term_grace = Duration::from_millis(80);
        config.eof_kill_grace = Duration::from_millis(200);
        let run_spec = spec_for(dir.path(), "anything");
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        // Let the stub's one assistant frame land before tearing down — it never reaches a
        // `result`, so there is nothing to `turn()` on; `finish()` is what this test exercises.
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        // The stub ignores EOF and only reacts to the SIGTERM `finish()` sends after
        // eof_term_grace — proves the escalation actually fired rather than idly waiting out
        // the (much longer, real) default grace periods.
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_wall_clock_timeout_kills_the_process_and_fails_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config(mock_script("mock-claude.mjs"));
        let mut run_spec = spec_for(dir.path(), "please wait mock:slow");
        run_spec.timeout_ms = Some(300);
        let mut session = open_claude_session(&config, &run_spec, &BTreeMap::new()).unwrap();
        let (outcome, events) = run_turn(&mut session);
        let error = outcome.expect_err("a hung turn should fail rather than park forever");
        assert!(error.contains("timed out"), "unexpected message: {error}");
        assert!(events.iter().any(|event| event.event_type == "error"));
    }

    #[test]
    fn spawn_failure_reports_a_friendly_missing_binary_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = ClaudeSpawnConfig {
            program: "coducktor-test-nonexistent-binary-xyz".to_owned(),
            ..Default::default()
        };
        let run_spec = spec_for(dir.path(), "hi");
        let error = match open_claude_session(&config, &run_spec, &BTreeMap::new()) {
            Err(error) => error,
            Ok(_) => panic!("expected spawn to fail"),
        };
        assert!(
            error.contains("not found on PATH"),
            "unexpected message: {error}"
        );
    }
}
