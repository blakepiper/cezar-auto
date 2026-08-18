//! `AgentSession` over `opencode serve` — the local OpenCode process with an SSE event stream.
//!
//! Auth is the host's opencode config/logins. The agent runs autonomously (auto-approved
//! permissions); OpenCode has no per-tool allowlist, so `spec.allowed_tools`/`bash_allowlist` are
//! ignored. `spec.model` is `provider/model`, split via [`crate::model_identity`].
//!
//! # Architecture notes
//!
//! Same turn-scoped adaptation as the claude/codex backends — see `claude_runner`'s doc for the
//! general shape. Unlike codex's stdio JSON-RPC (every bootstrap step is a roundtrip over the
//! SAME channel live notifications arrive on, forcing deferral into the first `turn()` call),
//! HTTP requests are self-contained: spawning, reading the bound URL back off stdout, creating
//! the session, and connecting the SSE stream none of them need a live read/dispatch loop to
//! resolve. So [`open_opencode_session`] does ALL of that eagerly, much like claude's — only
//! emitting the `"session"` event and sending the opening prompt wait for the first `turn()` call
//! (which is what supplies an event sink).
//!
//! **The prompt POST and the SSE stream run concurrently on purpose**, matching the real
//! local process behavior the bundled test mock deliberately reproduces: the POST response can
//! resolve BEFORE the turn's final SSE parts arrive. Here, [`OpencodeSession::post_and_drain`] spawns the POST on its
//! own thread and merges it with the SSE channel on the calling thread — draining whatever has
//! arrived, checking whether the POST has finished, in a short poll loop — so `on_event` still
//! sees each part live rather than batched after the fact. `v1`'s `turn-end` is synthesized once
//! that POST settles (**not** from the SSE `session.idle`) — any part that hasn't reached
//! `time.end` by then is flushed as-is.
//!
//! `text_seen`/`tools_seen`/the coalescer are session-scoped fields here (not reset per turn),
//! matching the session's instance fields — a part id is assumed unique for
//! the life of the session, not just one turn. `text_chunks` is session-scoped too; each turn's
//! [`coducktor_core::workflows::run::SessionReport::turn_text`] is a slice of it from an index
//! captured at that turn's start, and token/cost totals are session-cumulative counters with the
//! same before/after snapshot taken per turn — the wire's own `tokens`/`cost` fields are already
//! cumulative-over-the-session, not per-turn deltas.
//!
//! One narrow, deliberate behavior difference, matching the same call already made for codex:
//! A follow-up prompt failure is returned as a hard `Err`. A second, opencode-specific behavior:
//! the wall-clock timeout sends SIGTERM (`interrupt()`) and then SIGKILL
//! follow-up if the process ignores it — genuinely able to hang the whole `result` promise
//! forever. This session escalates to SIGKILL after `kill_grace`, same as claude/codex's own
//! timeout paths, closing that gap rather than reproducing it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
use std::sync::LazyLock;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use coducktor_contract::Runner;
use coducktor_core::runs::ask;
use coducktor_core::workflows::run::{
    AgentSession, EventInput, PromptImage, SessionOutcome, SessionReport, TurnMarkerDecision,
    decide_turn_marker,
};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::agent_runner::{AgentRunSpec, ContentBlock, reasoning_effort_str};
use crate::child_process::{ChildProcess, NextLine, SpawnConfig};
use crate::claude_runner::{DEFAULT_RUN_TIMEOUT_MS, EOF_KILL_GRACE_MS};
use crate::model_identity::parse_model_identity;
use crate::v1_text_coalescer::V1TextCoalescer;
use crate::wire::json_string;

const SERVER_START_TIMEOUT_MS: u64 = 30_000;

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[\d.]+:\d+").expect("fixed pattern"));
static VARIANT_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)variant|reasoning|model.*(not found|invalid|unsupported)")
        .expect("fixed pattern")
});

/// Where to find the opencode binary. Production wiring resolves `program`/`prefix_args` from
/// `DUCK_OPENCODE_BIN` is resolved by the session factory; tests point `program` at
/// `node` with `prefix_args: vec![mock_script_path]`.
#[derive(Debug, Clone)]
pub struct OpencodeSpawnConfig {
    pub program: String,
    pub prefix_args: Vec<String>,
    /// Grace period after `finish()`'s SIGTERM before escalating to SIGKILL, and after a
    /// wall-clock timeout's SIGTERM too.
    pub kill_grace: Duration,
}

impl Default for OpencodeSpawnConfig {
    fn default() -> Self {
        Self {
            program: "opencode".to_owned(),
            prefix_args: Vec::new(),
            // TS hardcodes 4_000ms directly in `end()`; reusing claude's identically-valued
            // constant here is coincidental, not a shared policy — opencode has no EOF watchdog.
            kill_grace: Duration::from_millis(EOF_KILL_GRACE_MS),
        }
    }
}

fn wrap_spawn_error(error: &io::Error, program: &str) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!(
            "`{program}` not found on PATH — install OpenCode (https://opencode.ai) and run `opencode` once to configure a provider"
        )
    } else {
        error.to_string()
    }
}

fn pick_port() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    40_000 + (nanos % 20_000) as u16
}

fn is_model_variant_error(message: &str) -> bool {
    VARIANT_ERROR_RE.is_match(message)
}

/// A live `opencode serve` session driving a single server-side session. Implements
/// [`AgentSession`].
pub struct OpencodeSession {
    process: ChildProcess,
    client: reqwest::blocking::Client,
    base_url: String,
    session_id: String,
    sse_rx: mpsc::Receiver<Value>,
    spec: AgentRunSpec,
    /// messageID -> role. Parts carry no role; only assistant parts are surfaced (the user's own
    /// message also streams as parts over the same SSE feed).
    msg_role: HashMap<String, String>,
    /// Per text-part cursor (character count) so only newly-appended text is buffered as deltas.
    text_seen: HashMap<String, usize>,
    tools_seen: HashSet<String>,
    coalescer: V1TextCoalescer,
    text_chunks: Vec<String>,
    tokens_used_total: f64,
    input_tokens_total: f64,
    output_tokens_total: f64,
    cost_total: f64,
    /// Whether the OpenCode session is still open.
    open: bool,
    /// 0 disables the wall-clock kill switch entirely (interactive sessions).
    timeout_ms: u64,
    kill_grace: Duration,
}

/// Spawn `opencode serve`, read its bound URL back off stdout, create an OpenCode session, and
/// connect the SSE stream — all eagerly (see the module doc for why this differs from codex's
/// deferred bootstrap). Nothing is sent to the model yet; the first `turn()` call does that.
pub fn open_opencode_session(
    config: &OpencodeSpawnConfig,
    spec: AgentRunSpec,
    host_env: &BTreeMap<String, String>,
) -> Result<OpencodeSession, String> {
    let port = pick_port();
    let mut args = config.prefix_args.clone();
    args.extend([
        "serve".to_owned(),
        "--hostname".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
    ]);
    let mut process = ChildProcess::spawn(
        &SpawnConfig {
            program: config.program.clone(),
            args,
            // No EOF watchdog for a process that reads no stdin protocol.
            eof_term_grace: Duration::ZERO,
            eof_kill_grace: Duration::ZERO,
            kill_grace: config.kill_grace,
        },
        Runner::OpenCode,
        &spec.cwd,
        &spec.env,
        host_env,
    )
    .map_err(|error| wrap_spawn_error(&error, &config.program))?;
    process.set_cancellation(spec.cancellation.clone());

    let base_url = wait_for_server_url(&mut process, port);
    // stdout is only ever used for that one startup line — keep draining it in the background
    // so neither the channel nor the underlying OS pipe backs up over a long session.
    process.discard_stdout();

    let client = reqwest::blocking::Client::new();

    let created = http_post(
        &client,
        &format!("{base_url}/session"),
        &json!({"title": "coducktor task"}),
    )?;
    let session_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "opencode did not return a session id".to_owned())?
        .to_owned();

    // The SSE subscription must be live BEFORE the first prompt posts — events the server emits
    // while that POST is in flight would otherwise be lost.
    let sse_rx = connect_sse(&client, &base_url)?;

    let timeout_ms = spec.timeout_ms.unwrap_or(DEFAULT_RUN_TIMEOUT_MS);

    Ok(OpencodeSession {
        process,
        client,
        base_url,
        session_id,
        sse_rx,
        spec,
        msg_role: HashMap::new(),
        text_seen: HashMap::new(),
        tools_seen: HashSet::new(),
        coalescer: V1TextCoalescer::new(),
        text_chunks: Vec::new(),
        tokens_used_total: 0.0,
        input_tokens_total: 0.0,
        output_tokens_total: 0.0,
        cost_total: 0.0,
        open: true,
        timeout_ms,
        kill_grace: config.kill_grace,
    })
}

fn wait_for_server_url(process: &mut ChildProcess, fallback_port: u16) -> String {
    let deadline = Instant::now() + Duration::from_millis(SERVER_START_TIMEOUT_MS);
    while let Ok(NextLine::Line(line)) = process.next_line(Some(deadline)) {
        if let Some(found) = URL_RE.find(&line) {
            return found.as_str().to_owned();
        }
    }
    format!("http://127.0.0.1:{fallback_port}")
}

fn http_post(client: &reqwest::blocking::Client, url: &str, body: &Value) -> Result<Value, String> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .map_err(|error| error.to_string())?;
    read_json_response(url, response)
}

fn read_json_response(url: &str, response: reqwest::blocking::Response) -> Result<Value, String> {
    let status = response.status();
    let text = response.text().unwrap_or_default();
    if !status.is_success() {
        let detail: String = text.chars().take(200).collect();
        return Err(format!("POST {url} → {status} {detail}"));
    }
    if text.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Object(Map::new())))
}

/// Connect the SSE stream on a background thread and block until it is either connected (headers
/// received) or fails — mirrors `consumeEvents()` resolving once connected, frames drained in the
/// background afterward.
fn connect_sse(
    client: &reqwest::blocking::Client,
    base_url: &str,
) -> Result<mpsc::Receiver<Value>, String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (tx, rx) = mpsc::channel::<Value>();
    let client = client.clone();
    let url = format!("{base_url}/event");
    thread::spawn(move || {
        let response = client
            .get(&url)
            .header("accept", "text/event-stream")
            .send();
        let response = match response {
            Ok(response) if response.status().is_success() => {
                let _ = ready_tx.send(Ok(()));
                response
            }
            Ok(response) => {
                let _ = ready_tx.send(Err(format!("GET {url} → {}", response.status())));
                return;
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error.to_string()));
                return;
            }
        };
        let mut reader = BufReader::new(response);
        let mut data_lines: Vec<String> = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\n', '\r']);
                    if trimmed.is_empty() {
                        if !data_lines.is_empty() {
                            let payload = data_lines.join("\n");
                            data_lines.clear();
                            if let Ok(value) = serde_json::from_str::<Value>(&payload)
                                && tx.send(value).is_err()
                            {
                                return;
                            }
                        }
                    } else if let Some(rest) = trimmed.strip_prefix("data:") {
                        data_lines.push(rest.trim().to_owned());
                    }
                }
                Err(_) => break,
            }
        }
    });
    match ready_rx.recv_timeout(Duration::from_millis(SERVER_START_TIMEOUT_MS)) {
        Ok(Ok(())) => Ok(rx),
        Ok(Err(message)) => Err(message),
        Err(_) => Err("opencode SSE stream did not connect in time".to_owned()),
    }
}

impl OpencodeSession {
    fn build_prompt_body(
        &self,
        text: &str,
        images: &[PromptImage],
        include_variant: bool,
        system_prompt: Option<&str>,
    ) -> Value {
        let mut body = Map::new();
        body.insert(
            "parts".to_owned(),
            Value::Array(opencode_parts(text, images)),
        );
        if include_variant && let Some(effort) = self.spec.reasoning_effort {
            body.insert("variant".to_owned(), json!(reasoning_effort_str(effort)));
        }
        if let Some(system_prompt) = system_prompt.filter(|prompt| !prompt.is_empty()) {
            body.insert("system".to_owned(), json!(system_prompt));
        }
        // `spec.model` arrives already normalised to canonical `provider/model`; split it into
        // opencode's `{ providerID, modelID }`.
        if let Some(identity) = parse_model_identity(self.spec.model.as_deref()) {
            body.insert(
                "model".to_owned(),
                json!({"providerID": identity.provider, "modelID": identity.model}),
            );
        }
        Value::Object(body)
    }

    /// Post one prompt on its own thread and merge it with the SSE channel on this thread — a
    /// short poll loop that drains whatever SSE frames have arrived, dispatching each live, then
    /// checks whether the POST has finished. Returns the POST's parsed JSON body.
    fn post_and_drain(
        &mut self,
        path: &str,
        body: Value,
        deadline: Option<Instant>,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<Value, String> {
        let url = format!("{}{}", self.base_url, path);
        let client = self.client.clone();
        let post_handle: thread::JoinHandle<Result<Value, String>> = thread::spawn(move || {
            let response = client
                .post(&url)
                .json(&body)
                .send()
                .map_err(|error| error.to_string())?;
            read_json_response(&url, response)
        });

        loop {
            if self.spec.cancellation.is_requested() {
                self.open = false;
                self.process.signal_term();
                let _ = post_handle.join();
                return Err("opencode run cancelled".to_owned());
            }
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                self.open = false;
                self.process.escalate_after_timeout();
                let _ = post_handle.join();
                let minutes = (self.timeout_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
                return Err(format!(
                    "opencode timed out after {minutes}m and was killed"
                ));
            }
            if let Ok(frame) = self.sse_rx.recv_timeout(Duration::from_millis(20)) {
                self.handle_frame(&frame, on_event)?;
            }
            if post_handle.is_finished() {
                break;
            }
        }
        // The POST resolving doesn't guarantee SSE frames sent moments earlier — a separate TCP
        // connection — have already been delivered to this process; keep draining as long as
        // frames keep actively arriving, stopping at the first quiet gap. Frames genuinely
        // queued for LATER (the real server, and this crate's own test mock, only stream a
        // turn's tail some tens of ms after acking the prompt) are still correctly excluded —
        // this is the same race the TS source's own concurrent read loop relies on timing to
        // win, just given a bounded, adaptive window instead of hoping the scheduler cooperates.
        while let Ok(frame) = self.sse_rx.recv_timeout(Duration::from_millis(15)) {
            self.handle_frame(&frame, on_event)?;
        }
        post_handle
            .join()
            .map_err(|_| "opencode prompt thread panicked".to_owned())?
    }

    /// One prompt round trip, with the model-variant fallback retry. Any failure — including a
    /// wall-clock timeout — is reported as a single `error` event here (`post_and_drain` itself
    /// never emits one, so a retried timeout can't double-report).
    fn run_prompt(
        &mut self,
        text: &str,
        images: &[PromptImage],
        system_prompt: Option<&str>,
        deadline: Option<Instant>,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        let path = format!("/session/{}/message", self.session_id);
        let body = self.build_prompt_body(text, images, true, system_prompt);
        let result = self.post_and_drain(&path, body, deadline, on_event);
        // OpenCode variants are model-specific; a generic Auto decision can name a level the
        // selected model doesn't advertise. Retry once with the provider default instead of
        // turning a valid task into a hard failure.
        let result = match result {
            Err(message)
                if self.spec.reasoning_effort.is_some() && is_model_variant_error(&message) =>
            {
                let effort = self.spec.reasoning_effort.expect("checked above");
                on_event(EventInput::new("note").field(
                    "message",
                    format!(
                        "opencode: reasoning variant \"{}\" is unavailable; using the model default",
                        reasoning_effort_str(effort)
                    ),
                ))
                .map_err(|error| error.to_string())?;
                let fallback_body = self.build_prompt_body(text, images, false, system_prompt);
                self.post_and_drain(&path, fallback_body, deadline, on_event)
            }
            other => other,
        };
        match result {
            Ok(response) => {
                self.absorb_usage(&response, on_event)?;
                Ok(())
            }
            Err(message) => {
                on_event(EventInput::new("error").field("message", message.clone()))
                    .map_err(|error| error.to_string())?;
                Err(message)
            }
        }
    }

    /// One full turn: send `text` (the opening prompt, or a follow-up), then flush any
    /// still-buffered prose and emit `turn-end` regardless of outcome — matching
    /// `prompt()`'s TS `finally` block, which runs even when the prompt itself failed.
    fn run_one_turn(
        &mut self,
        text: &str,
        images: &[PromptImage],
        system_prompt: Option<&str>,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        let deadline =
            (self.timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(self.timeout_ms));
        let baseline = (
            self.tokens_used_total,
            self.input_tokens_total,
            self.output_tokens_total,
            self.cost_total,
        );
        let text_start = self.text_chunks.len();

        let result = self.run_prompt(text, images, system_prompt, deadline, on_event);
        // A part that never saw `time.end` (abort, server quirk) still surfaces its prose before
        // the turn boundary.
        for flushed in self.coalescer.flush() {
            self.text_chunks.push(flushed.clone());
            on_event(EventInput::new("text").field("text", flushed))
                .map_err(|error| error.to_string())?;
        }
        on_event(EventInput::new("turn-end")).map_err(|error| error.to_string())?;
        result?;
        self.finalize_turn(baseline, text_start)
    }

    fn finalize_turn(
        &self,
        baseline: (f64, f64, f64, f64),
        text_start: usize,
    ) -> Result<SessionOutcome, String> {
        let turn_text = self.text_chunks[text_start..].join("\n").trim().to_owned();
        let valid_ask = ask::parse_ask_marker(&turn_text).is_some();
        let decision = decide_turn_marker(&turn_text, self.open, valid_ask);
        let cost_delta = self.cost_total - baseline.3;
        let report = SessionReport {
            session_id: Some(self.session_id.clone()),
            tokens_used: self.tokens_used_total - baseline.0,
            input_tokens: Some(self.input_tokens_total - baseline.1),
            output_tokens: Some(self.output_tokens_total - baseline.2),
            cost_usd: (cost_delta > 0.0).then_some(cost_delta),
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

    fn handle_frame(
        &mut self,
        evt: &Value,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        let event_type = evt.get("type").and_then(Value::as_str).unwrap_or("");
        let props = evt.get("properties").cloned().unwrap_or(Value::Null);
        match event_type {
            "message.updated" | "message.created" | "message.completed" => {
                let info = props.get("info").cloned().unwrap_or_else(|| props.clone());
                let message_id = info.get("id").and_then(Value::as_str).map(str::to_owned);
                let role = info.get("role").and_then(Value::as_str).map(str::to_owned);
                if let (Some(mid), Some(role)) = (&message_id, &role) {
                    self.msg_role.insert(mid.clone(), role.clone());
                }
                self.absorb_usage(&info, on_event)?;
            }
            "message.part.updated" | "message.part.created" => {
                let part = props.get("part").cloned().unwrap_or(props);
                self.handle_part(&part, on_event)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Only surface parts of assistant messages — the user's own message streams over the same
    /// feed. Role is known early (`message.updated` precedes its parts); an unknown role means
    /// "not assistant yet" -> skip.
    fn handle_part(
        &mut self,
        part: &Value,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        let message_id = part
            .get("messageID")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(mid) = &message_id
            && self.msg_role.get(mid).map(String::as_str) != Some("assistant")
        {
            return Ok(());
        }
        let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
        let id = part
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| message_id.clone())
            .unwrap_or_default();

        if kind == "text" {
            let full = part.get("text").and_then(Value::as_str).unwrap_or("");
            let full_len = full.chars().count();
            let seen = *self.text_seen.get(&id).unwrap_or(&0);
            if full_len > seen {
                self.text_seen.insert(id.clone(), full_len);
                let delta: String = full.chars().skip(seen).collect();
                self.coalescer.append(Some(&id), &delta);
            }
            // `time.end` marks the part finished — emit the whole block once, preferring the
            // snapshot's full text.
            let ended = part.pointer("/time/end").and_then(Value::as_f64).is_some();
            if ended && let Some(text) = self.coalescer.complete(Some(&id), Some(full)) {
                self.text_chunks.push(text.clone());
                on_event(EventInput::new("text").field("text", text))
                    .map_err(|error| error.to_string())?;
            }
        } else if kind == "reasoning" {
            let text = part
                .get("text")
                .or_else(|| part.get("reasoning"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let ended = part.pointer("/time/end").and_then(Value::as_f64).is_some();
            if ended && !text.is_empty() {
                on_event(EventInput::new("reasoning").field("text", text))
                    .map_err(|error| error.to_string())?;
            }
        } else if kind == "tool" {
            let state = part.get("state").cloned().unwrap_or(Value::Null);
            let status = state.get("status").and_then(Value::as_str).unwrap_or("");
            let name = part
                .get("tool")
                .and_then(Value::as_str)
                .or_else(|| part.get("name").and_then(Value::as_str))
                .unwrap_or("tool")
                .to_owned();
            let call_id = if id.is_empty() {
                format!("{name}-{}", self.tools_seen.len())
            } else {
                id.clone()
            };
            if self.tools_seen.insert(call_id.clone()) {
                let input = state.get("input").cloned().unwrap_or_else(|| state.clone());
                on_event(
                    EventInput::new("tool-call")
                        .field("id", &call_id)
                        .field("tool", &name)
                        .field("input", input),
                )
                .map_err(|error| error.to_string())?;
            }
            if status == "completed" || status == "error" {
                let output = state
                    .get("output")
                    .or_else(|| state.get("result"))
                    .cloned()
                    .unwrap_or_else(|| state.clone());
                let result_text = match &output {
                    Value::String(text) => text.clone(),
                    other => json_string(other),
                };
                on_event(
                    EventInput::new("tool-result")
                        .field("toolCallId", &call_id)
                        .field("result", result_text)
                        .field("isError", status == "error"),
                )
                .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    /// Pull cumulative tokens/cost out of an assistant message info object — the wire's own
    /// `tokens`/`cost` fields are running totals over the whole session, not per-turn deltas, so
    /// this only updates (and emits) when the total actually grew.
    fn absorb_usage(
        &mut self,
        info: &Value,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        if let Some(tokens) = info.get("tokens") {
            let input = tokens.get("input").and_then(Value::as_f64).unwrap_or(0.0);
            let output = tokens.get("output").and_then(Value::as_f64).unwrap_or(0.0);
            let reasoning = tokens
                .get("reasoning")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let total = input + output + reasoning;
            if total > self.tokens_used_total {
                self.tokens_used_total = total;
                self.input_tokens_total = input;
                self.output_tokens_total = output;
                on_event(EventInput::new("token-usage").field("tokensUsed", total))
                    .map_err(|error| error.to_string())?;
            }
        }
        let cost = info.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        if cost > self.cost_total {
            let delta = cost - self.cost_total;
            on_event(EventInput::new("cost").field("usd", delta))
                .map_err(|error| error.to_string())?;
            self.cost_total = cost;
        }
        Ok(())
    }
}

fn opencode_parts(text: &str, images: &[PromptImage]) -> Vec<Value> {
    let mut parts = Vec::with_capacity(images.len() + usize::from(!text.is_empty()));
    for image in images {
        parts.push(json!({
            "type": "file",
            "mime": image.media_type,
            "url": image.data_url(),
        }));
    }
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    parts
}

impl AgentSession for OpencodeSession {
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if !self.open {
            return Err("session is closed".to_owned());
        }
        on_event(EventInput::new("session").field("sessionId", self.session_id.clone()))
            .map_err(|error| error.to_string())?;
        let first_text = self.spec.user_prompt.clone();
        let images = self
            .spec
            .images
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image { source } => Some(PromptImage {
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                }),
                ContentBlock::Text { .. } => None,
            })
            .collect::<Vec<_>>();
        let system_prompt = self.spec.system_prompt.clone();
        self.run_one_turn(&first_text, &images, system_prompt.as_deref(), on_event)
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
        self.run_one_turn(prompt, images, None, on_event)
    }

    fn finish(
        &mut self,
        _on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        if self.open {
            self.open = false;
            self.process.escalate_immediately(self.kill_grace);
        }
        self.process.wait_for_exit();
        Ok(SessionOutcome::Completed(SessionReport::default()))
    }

    fn cancel(&mut self) {
        self.open = false;
        // Best-effort graceful cancel (fire-and-forget, matching TS's own
        // `.catch(() => undefined)`; there is no event sink here to react to its response),
        // then a hard stop.
        let _ = self
            .client
            .post(format!(
                "{}/session/{}/abort",
                self.base_url, self.session_id
            ))
            .send();
        if !self.process.has_exited() {
            self.process.signal_term();
        }
    }

    fn session_id(&self) -> Option<String> {
        Some(self.session_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_parts_place_data_url_files_before_text() {
        let image = PromptImage {
            media_type: "image/png".to_owned(),
            data: "AQID".to_owned(),
        };
        assert_eq!(
            opencode_parts("inspect", &[image]),
            vec![
                json!({"type": "file", "mime": "image/png", "url": "data:image/png;base64,AQID"}),
                json!({"type": "text", "text": "inspect"}),
            ]
        );
    }

    #[test]
    fn opening_prompt_uses_opencodes_native_system_field() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let mut run_spec = spec_for(dir.path(), "answer the question");
        run_spec.system_prompt = Some("Follow the task controls.".to_owned());
        let mut session = open_opencode_session(&config, run_spec, &BTreeMap::new()).unwrap();

        let body = session.build_prompt_body(
            "answer the question",
            &[],
            true,
            Some("Follow the task controls."),
        );

        assert_eq!(
            body.get("system").and_then(Value::as_str),
            Some("Follow the task controls.")
        );
        assert_eq!(
            body.pointer("/parts/0/text").and_then(Value::as_str),
            Some("answer the question")
        );
        session.finish(&mut |_| Ok(())).unwrap();
    }
    use std::path::PathBuf;

    fn mock_script() -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../fixtures/opencode/mock-opencode-serve.mjs");
        path.canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn node_config() -> OpencodeSpawnConfig {
        OpencodeSpawnConfig {
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

    fn run_turn(
        session: &mut OpencodeSession,
    ) -> (Result<SessionOutcome, String>, Vec<EventInput>) {
        let mut events = Vec::new();
        let result = session.turn(&mut |event| {
            events.push(event);
            Ok(())
        });
        (result, events)
    }

    #[test]
    fn parse_model_identity_is_reachable_and_pure() {
        // Smoke check that the shared helper is wired correctly from this module's own imports.
        assert!(parse_model_identity(Some("anthropic/claude")).is_some());
        assert!(parse_model_identity(None).is_none());
    }

    #[test]
    fn is_model_variant_error_matches_the_documented_shapes() {
        assert!(is_model_variant_error(
            "unknown variant \"high\" for model x"
        ));
        assert!(is_model_variant_error("model gpt-5 not found"));
        assert!(!is_model_variant_error("connection refused"));
    }

    #[test]
    fn a_first_turn_against_the_mock_streams_the_expected_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "check the working tree");
        let mut session = open_opencode_session(&config, run_spec, &BTreeMap::new()).unwrap();
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
        assert!(event_types.contains(&"cost"));
        assert!(event_types.contains(&"turn-end"));

        // The mock never sets `time.end` on the first text part, so it only surfaces via the
        // end-of-turn flush — and the LATE "Done." part (sent after the HTTP response, see the
        // mock's own comment) must NOT appear: v1's turn-end is synthesized from that response,
        // not from `session.idle`.
        match outcome {
            SessionOutcome::Waiting(report) => {
                assert!(report.turn_text.contains("Checking the working tree."));
                assert!(!report.turn_text.contains("Done."));
                assert_eq!(report.tokens_used, 1500.0);
                assert_eq!(report.input_tokens, Some(1200.0));
                assert_eq!(report.output_tokens, Some(300.0));
                assert_eq!(report.cost_usd, Some(0.0021));
            }
            other => panic!("expected Waiting, got {other:?}"),
        }

        let tool_call = events
            .iter()
            .find(|event| event.event_type == "tool-call")
            .expect("a tool-call event");
        assert_eq!(
            tool_call.extra.get("tool").and_then(Value::as_str),
            Some("bash")
        );
        let tool_result = events
            .iter()
            .find(|event| event.event_type == "tool-result")
            .expect("a tool-result event");
        assert_eq!(
            tool_result.extra.get("result").and_then(Value::as_str),
            Some(" M src/example.ts\n")
        );
        assert_eq!(
            tool_result.extra.get("isError").and_then(Value::as_bool),
            Some(false)
        );

        session.finish(&mut |_| Ok(())).unwrap();
    }

    #[test]
    fn finish_closes_a_cooperative_process_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let config = node_config();
        let run_spec = spec_for(dir.path(), "hello");
        let mut session = open_opencode_session(&config, run_spec, &BTreeMap::new()).unwrap();
        run_turn(&mut session)
            .0
            .expect("first turn should complete");
        let started = Instant::now();
        let outcome = session.finish(&mut |_| Ok(())).unwrap();
        assert!(matches!(outcome, SessionOutcome::Completed(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn spawn_failure_reports_a_friendly_missing_binary_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = OpencodeSpawnConfig {
            program: "coducktor-test-nonexistent-binary-xyz".to_owned(),
            ..Default::default()
        };
        let run_spec = spec_for(dir.path(), "hi");
        let error = match open_opencode_session(&config, run_spec, &BTreeMap::new()) {
            Err(error) => error,
            Ok(_) => panic!("expected spawn to fail"),
        };
        assert!(
            error.contains("not found on PATH"),
            "unexpected message: {error}"
        );
    }
}
