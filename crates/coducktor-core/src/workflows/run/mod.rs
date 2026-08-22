//! Backend-neutral run management.
//!
//! The manager keeps a project-local map of [`RunRecord`] values in front of the `runs.json` and
//! NDJSON primitives, publishes updates to in-process observers, and ensures durable state is
//! written before the call returns. It does not know about the TUI or any concrete runner.
//!
//! The queue, marker, review, account-hold, and prompt helpers below are intentionally pure. They
//! are the small decisions the eventual session/lifecycle modules can share without recreating
//! the run lifecycle rules in each caller.

pub mod auto_resume;
pub mod context_refresh;
pub mod lifecycle;
pub mod monitoring;
pub mod quota;
pub mod recovery;
pub mod review_gate;
pub mod semaphore;
pub mod session;
pub mod variants;

pub use quota::{MAX_AUTO_RESUME_ATTEMPTS, QuotaReconciliation, reconcile_quota};
pub use review_gate::{enabled as review_gate_enabled, settle_status as success_status};
pub use semaphore::{RepositoryRootLease, WorkspaceSemaphore};
pub use session::{
    MAX_AUTONOMOUS_CONTINUES, TurnMarkerDecision, append_turn_text, decide_turn_marker,
    strip_turn_marker, system_prompt_with_task_controls,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use coducktor_contract::events::RunEvent;
use coducktor_contract::runs::{
    MarkerRefs, RunActivity, RunRecord, RunStatus, StepKind, StepState, StepStatus,
};
use coducktor_contract::workflows::WorkflowDef;
use coducktor_contract::{
    ConcreteReasoningEffort, ReasoningEffort, RoutingDecision, Runner, RunnerSelection,
};
use serde::Serialize;
use serde_json::{Map, Value};

use super::types;
use crate::runs::events;
use crate::runs::store;
use crate::runs::task_markers::{self, TaskMarkers};
use crate::time::{is_zod_datetime, now_iso8601, now_plus_iso8601};

/// The exact nudge text sent to an autonomous session that appears to have finished without the
/// completion marker. `pub` so a caller driving [`TurnStep::Nudge`] outside this module (a
/// per-run worker) sends byte-for-byte the same prompt this module's own synchronous resume path
/// uses.
pub const AUTONOMOUS_NUDGE: &str = "Your immediately preceding response may already have completed the user's original request, but it did not include the required completion marker. Do not begin new work, search for unrelated work, or expand the task. If the original request is fully complete, reply with exactly DUCK:DONE. Otherwise, continue only the original request. If you genuinely need user input, end normally without a marker.";

/// Sent after a `git_auto` run finishes its work with a diff. The dispatcher uses the returned
/// subject with its normal Git helpers; Git itself remains outside this backend-neutral crate.
pub const AUTOMATIC_COMMIT_MESSAGE_NUDGE: &str = "The task is complete and its changes will now be committed automatically. Reply with only one concise, imperative Git commit subject (72 characters or fewer). Do not use Markdown, quotes, a body, or the DUCK completion marker.";

/// Sent to a parked monitoring session once its durable `monitoringWakeAt` deadline passes.
/// `pub` for the same reason as `AUTONOMOUS_NUDGE`: the caller that actually sends it
/// (`RunManager::begin_monitoring_wake`'s caller, outside any lock) needs the exact text.
pub const MONITORING_WAKE_PROMPT: &str = "Periodic monitoring check-in: reassess whatever you are watching for and report back. If the condition you were waiting for is now met, finish and reply with exactly DUCK:DONE. Otherwise, do not begin new work — just note the current state and continue waiting.";

/// A patch represented with the same camelCase keys as the persisted contract.
///
/// A JSON patch is used here rather than duplicating the very wide `RunRecord` shape in a second
/// Rust struct. `RunManager` validates the merged value by deserializing it back into the shared
/// contract type before it can be stored. `null` clears an optional record field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunPatch {
    fields: Map<String, Value>,
}

impl RunPatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a patch from an object-shaped JSON value.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let Value::Object(fields) = value else {
            return Err("run patch must be a JSON object".to_owned());
        };
        Ok(Self { fields })
    }

    /// Add or replace a typed camelCase field. Panicking here would only be possible for a type
    /// that cannot be represented by JSON; all contract values used by this module are JSON types.
    pub fn set<T: Serialize>(mut self, field: &str, value: T) -> Self {
        self.fields.insert(
            field.to_owned(),
            serde_json::to_value(value).unwrap_or(Value::Null),
        );
        self
    }

    /// Clear an optional field. Required fields will be rejected by the contract deserializer.
    pub fn clear(mut self, field: &str) -> Self {
        self.fields.insert(field.to_owned(), Value::Null);
        self
    }

    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }
}

/// Step patches use the same JSON-field representation as [`RunPatch`].
pub type StepPatch = RunPatch;

/// Input for durable run creation. `steps` is the compact execution-facing subset of a workflow
/// step; the complete ad-hoc definition can be retained in `workflow_def`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreateRunInput {
    pub title: String,
    pub workflow: String,
    pub task: String,
    pub task_images: Option<Vec<String>>,
    pub steps: Vec<StepSeed>,
    pub workflow_def: Option<WorkflowDef>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub model_identity: Option<String>,
    pub runner: Option<Runner>,
    pub requested_runner: Option<RunnerSelection>,
    pub agent_profile: Option<String>,
    pub system_prompt: Option<String>,
    pub autonomous: Option<bool>,
    pub git_auto: Option<bool>,
    pub worktree: Option<bool>,
    pub group_id: Option<String>,
    pub variant: Option<String>,
    /// Explanation for an `auto` runner request, attached to the run's first step. `None` for an
    /// explicit runner request or when the caller has no decision to record.
    pub routing_decision: Option<RoutingDecision>,
}

impl CreateRunInput {
    /// Construct creation input from the shared workflow definition. This is metadata only: it
    /// does not execute any step or resolve a backend.
    pub fn from_workflow(workflow: &WorkflowDef, task: impl Into<String>) -> Self {
        let task = task.into();
        let title = task
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .unwrap_or(workflow.name.as_str())
            .to_owned();
        let steps = workflow
            .steps
            .iter()
            .map(|step| StepSeed {
                id: step.id.clone(),
                name: step.name.clone().unwrap_or_else(|| step.id.clone()),
                kind: types::step_kind(step),
                requested_runner: step.runner,
            })
            .collect();
        Self {
            title,
            workflow: workflow.name.clone(),
            task,
            steps,
            workflow_def: Some(workflow.clone()),
            ..Self::default()
        }
    }
}

/// The fields that are present when a new step is added to a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepSeed {
    pub id: String,
    pub name: String,
    pub kind: StepKind,
    pub requested_runner: Option<RunnerSelection>,
}

/// An event before the manager allocates its durable sequence and timestamp.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventInput {
    pub event_type: String,
    pub step_id: Option<String>,
    pub extra: Map<String, Value>,
}

impl EventInput {
    pub fn new(event_type: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            ..Self::default()
        }
    }

    pub fn field<T: Serialize>(mut self, name: &str, value: T) -> Self {
        self.extra.insert(
            name.to_owned(),
            serde_json::to_value(value).unwrap_or(Value::Null),
        );
        self
    }

    pub fn step(mut self, step_id: String) -> Self {
        self.step_id = Some(step_id);
        self
    }
}

/// The event delivered to an in-process event observer.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEventNotification {
    pub run_id: String,
    pub event: RunEvent,
}

/// The run delivered to an in-process run observer.
pub type RunObserverId = u64;
pub type EventObserverId = u64;
type EventObservers = BTreeMap<EventObserverId, Box<dyn Fn(&RunEventNotification) + Send + Sync>>;
type RunObservers = BTreeMap<RunObserverId, Box<dyn Fn(&RunRecord) + Send + Sync>>;

/// Backend-neutral usage checkpoints emitted by the session layer.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageEvent {
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        input_tokens: Option<f64>,
        output_tokens: Option<f64>,
    },
}

#[derive(Debug, Clone)]
struct UsageInvocation {
    step_id: String,
    observed: bool,
    started_turns: HashSet<String>,
    recorded_turns: HashSet<String>,
}

/// The two account-hold kinds used by auto-resume scheduling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountHolds {
    pub deadline: BTreeSet<String>,
    pub in_flight: BTreeSet<String>,
}

/// Queue state that keeps FIFO order and the dequeue-to-start accounting seam separate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueState {
    queue: VecDeque<String>,
    starting: BTreeSet<String>,
}

impl QueueState {
    pub fn enqueue(&mut self, run_id: impl Into<String>) -> bool {
        let run_id = run_id.into();
        if self.queue.iter().any(|queued| queued == &run_id) || self.starting.contains(&run_id) {
            return false;
        }
        self.queue.push_back(run_id);
        true
    }

    /// Move one queued id into the starting set. The set covers the period before a session has
    /// registered as active, keeping queue accounting explicit during startup.
    pub fn take_next(&mut self) -> Option<String> {
        let run_id = self.queue.pop_front()?;
        self.starting.insert(run_id.clone());
        Some(run_id)
    }

    pub fn finish_start(&mut self, run_id: &str) -> bool {
        self.starting.remove(run_id)
    }

    pub fn push_front(&mut self, run_id: impl Into<String>) {
        let run_id = run_id.into();
        if !self.queue.iter().any(|queued| queued == &run_id) && !self.starting.contains(&run_id) {
            self.queue.push_front(run_id);
        }
    }

    pub fn remove(&mut self, run_id: &str) -> bool {
        let before = self.queue.len();
        self.queue.retain(|queued| queued != run_id);
        let removed = self.queue.len() != before;
        self.starting.remove(run_id) || removed
    }

    pub fn queued(&self) -> impl Iterator<Item = &str> {
        self.queue.iter().map(String::as_str)
    }

    pub fn starting(&self) -> impl Iterator<Item = &str> {
        self.starting.iter().map(String::as_str)
    }

    pub fn is_queued(&self, run_id: &str) -> bool {
        self.queue.iter().any(|queued| queued == run_id)
    }

    pub fn is_starting(&self, run_id: &str) -> bool {
        self.starting.contains(run_id)
    }
}

/// Queue queued records by creation time, oldest first. Ties use the id for deterministic
/// recovery while preserving the normal append order for distinct timestamps.
pub fn fifo_run_ids(runs: &[RunRecord]) -> Vec<String> {
    let mut queued: Vec<&RunRecord> = runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .collect();
    queued.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    queued.into_iter().map(|run| run.id.clone()).collect()
}

/// Whether an unread badge should include this run. This is the same predicate used by
/// `mark_all_read`, kept pure so the sweep and readers cannot silently diverge.
pub fn is_unread(run: &RunRecord) -> bool {
    !run.archived
        && matches!(run.status, RunStatus::Done | RunStatus::Failed)
        && !(run.status == RunStatus::Failed && run.auto_resume_at.is_some())
        && run.finished_at.is_some()
        && match (&run.seen_at, &run.finished_at) {
            (None, Some(_)) => true,
            (Some(seen), Some(finished)) => seen < finished,
            _ => false,
        }
}

/// Fold the durable task and queued messages into the prompt sent at dequeue time. This is
/// read-only: the separate record fields remain the source of truth across restarts.
pub fn hydrate_queued_prompt(run: &RunRecord) -> String {
    std::iter::once(run.task.as_str())
        .chain(
            run.queued_messages
                .iter()
                .flatten()
                .map(|message| message.text.as_str()),
        )
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn commit_subject(text: &str) -> Result<String, String> {
    let subject = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches('`')
        .trim_matches('"')
        .trim();
    if subject.is_empty() {
        return Err("agent did not provide an automatic commit subject".to_owned());
    }
    if subject.chars().count() > 72 {
        return Err("automatic commit subject exceeds 72 characters".to_owned());
    }
    if subject.contains('\n') || subject.contains('\r') || subject.chars().any(char::is_control) {
        return Err("automatic commit subject contains invalid control characters".to_owned());
    }
    Ok(subject.to_owned())
}

fn hydrate_queued_images(run: &RunRecord) -> Vec<PromptImage> {
    run.task_images
        .iter()
        .flatten()
        .chain(
            run.queued_messages
                .iter()
                .flatten()
                .flat_map(|message| message.images.iter().flatten()),
        )
        .filter_map(|url| PromptImage::from_data_url(url))
        .collect()
}

/// The account a run occupies: concrete provider plus profile, with a caller-supplied fallback
/// for old queued records that have not resolved a provider yet.
pub fn run_account_key(run: &RunRecord, fallback_runner: Runner) -> String {
    format!(
        "{}:{}",
        runner_name(run.runner.unwrap_or(fallback_runner)),
        run.agent_profile.as_deref().unwrap_or("default")
    )
}

fn runner_name(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    }
}

fn runner_label(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "Claude",
        Runner::Codex => "Codex",
        Runner::OpenCode => "OpenCode",
        Runner::Pi => "Pi",
    }
}

fn routing_reason_label(reason: coducktor_contract::RoutingReasonCode) -> &'static str {
    use coducktor_contract::RoutingReasonCode;
    match reason {
        RoutingReasonCode::Selected => "selected",
        RoutingReasonCode::Considered => "considered",
        RoutingReasonCode::Disabled => "disabled",
        RoutingReasonCode::NotInstalled => "not installed",
        RoutingReasonCode::Disconnected => "disconnected",
        RoutingReasonCode::AuthError => "auth error",
        RoutingReasonCode::ReservedQuota => "reserved quota",
        RoutingReasonCode::HardExhausted => "quota exhausted",
        RoutingReasonCode::UnknownUsage => "usage unknown",
    }
}

/// A readable transcript note for a routing decision: one headline plus one indented line per
/// other candidate and its reason — every candidate the router actually looked at, not just the
/// winner, so "why not Claude?" is answered in the transcript itself rather than requiring a
/// dive into raw run state. The full structured decision remains persisted on the step
/// (`StepState::routing_decision`) and duplicated onto a `routing-decision` event for anything
/// that wants it typed rather than parsed from this text.
fn routing_decision_note(decision: &RoutingDecision) -> String {
    let others: Vec<String> = decision
        .considered
        .iter()
        .filter(|candidate| candidate.reason != coducktor_contract::RoutingReasonCode::Selected)
        .map(|candidate| {
            format!(
                "  {} — {}",
                runner_label(candidate.runner),
                routing_reason_label(candidate.reason)
            )
        })
        .collect();
    let headline = match &decision.selected {
        Some(selection) => format!("Auto routing · selected {}", runner_label(selection.runner)),
        None => "Auto routing · no eligible candidate".to_owned(),
    };
    if others.is_empty() {
        headline
    } else {
        format!("{headline}\n{}", others.join("\n"))
    }
}

fn is_auto_route_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "usage limit",
        "weekly limit",
        "rate limit",
        "quota",
        "capacity",
        "overloaded",
        "authentication",
        "authenticate",
        "oauth",
        "unauthorized",
        "401",
        "not found on path",
        "unavailable",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn auto_route_failure_reason(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if ["usage limit", "weekly limit", "rate limit", "quota"]
        .iter()
        .any(|needle| message.contains(needle))
    {
        "hit a usage limit"
    } else if [
        "authentication",
        "authenticate",
        "oauth",
        "unauthorized",
        "401",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    {
        "could not authenticate"
    } else {
        "was unavailable"
    }
}

/// A scheduled automatic resume is allowed through an in-flight hold, but a fresh run is not.
pub fn resume_in_flight(run: &RunRecord) -> bool {
    run.auto_resume_attempts.is_some()
        && matches!(run.status, RunStatus::Queued | RunStatus::Running)
}

/// Decide whether one run is blocked by account holds. A known deadline blocks resumes too; an
/// in-flight resume only blocks fresh work.
pub fn account_held_for(run: &RunRecord, holds: &AccountHolds, fallback_runner: Runner) -> bool {
    let key = run_account_key(run, fallback_runner);
    holds.deadline.contains(&key) || (holds.in_flight.contains(&key) && !resume_in_flight(run))
}

/// Derive account holds from durable records. ISO timestamps sort lexicographically, so this keeps
/// the helper dependency-free while matching the persisted `Date.toISOString()` shape.
pub fn derive_account_holds(runs: &[RunRecord], now: &str) -> AccountHolds {
    let mut holds = AccountHolds::default();
    for run in runs {
        let key = run_account_key(run, run.runner.unwrap_or(Runner::Claude));
        if run.status == RunStatus::Failed
            && let Some(deadline) = run.auto_resume_at.as_deref()
            && is_zod_datetime(deadline)
            && deadline > now
        {
            holds.deadline.insert(key);
        } else if resume_in_flight(run) {
            holds.in_flight.insert(key);
        }
    }
    holds
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoResumeReport {
    pub plan: QuotaReconciliation,
    pub requeued: Vec<String>,
    pub retired: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub queued: Vec<String>,
    pub settled: Vec<String>,
    pub resumed: Vec<String>,
    pub failed: Vec<String>,
}

/// Input accepted by [`RunManager::start_run`]. It deliberately contains policy and prompt data,
/// not a backend client or process handle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StartRunInput {
    pub task: String,
    pub images: Vec<PromptImage>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub runner: Option<RunnerSelection>,
    /// Concrete backend chosen for an authored `auto` request. The durable request remains
    /// `auto`, while execution and affinity use this provider.
    pub resolved_runner: Option<Runner>,
    /// Ordered concrete fallbacks for an authored `auto` request. This is process-local routing
    /// state: the durable record keeps the user's `auto` intent and the currently selected runner.
    pub auto_runner_candidates: Vec<Runner>,
    /// Why `resolved_runner`/`auto_runner_candidates` came out the way they did. Recorded on the
    /// run's first step so a user can see what else was considered and why it wasn't picked.
    pub routing_decision: Option<RoutingDecision>,
    pub agent_profile: Option<String>,
    pub system_prompt: Option<String>,
    pub autonomous: Option<bool>,
    pub git_auto: Option<bool>,
    pub worktree: Option<bool>,
}

/// The persisted override fields accepted by [`RunManager::continue_run`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContinueOptions {
    pub text: Option<String>,
    pub images: Vec<PromptImage>,
    pub runner: Option<RunnerSelection>,
    pub model: Option<String>,
}

/// Backend-neutral base64 image carried with a user prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptImage {
    pub media_type: String,
    pub data: String,
}

impl PromptImage {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }

    fn from_data_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("data:")?;
        let (media_type, data) = rest.split_once(";base64,")?;
        Some(Self {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueResult {
    pub ok: bool,
    pub error: Option<String>,
}

impl ContinueResult {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
        }
    }
}

/// A backend-neutral stop signal that can be set without borrowing the active session.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicU8>);

impl CancellationToken {
    pub fn request(&self) -> bool {
        loop {
            match self.0.load(Ordering::Acquire) {
                0 => {
                    if self
                        .0
                        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return true;
                    }
                }
                1 => return true,
                _ => return false,
            }
        }
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire) == 1
    }

    pub fn deactivate(&self) {
        self.0.store(2, Ordering::Release);
    }
}

impl PartialEq for CancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancellationToken {}

/// All information a backend-neutral session needs to open one turn.
///
/// This intentionally stays thin (run/step identity, prompt, backend routing) rather than
/// widening into the full `AgentRunSpec` a concrete backend ultimately needs (`cwd`,
/// `allowed_tools`, `system_prompt`, …) — those fields either already had a single source of
/// truth available at this call site (the workflow step, the run record) with nowhere else that
/// needed them, so they ride along here. Backend-only fields such as additional directories and
/// timeout policy remain a concrete `SessionFactory`'s job to default sensibly.
/// Extend this struct when a factory needs data that only the run manager can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequest {
    pub run_id: String,
    pub step_id: String,
    pub prompt: String,
    pub images: Vec<PromptImage>,
    pub runner: RunnerSelection,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub continuation: bool,
    /// Concrete durable profile affinity. Integration resolves its minimal environment without
    /// exposing credentials to core.
    pub agent_profile: Option<String>,
    pub env: BTreeMap<String, String>,
    /// The admitted worktree when isolation is enabled, otherwise the repository root.
    pub cwd: PathBuf,
    /// From `step.allowed_tools`, falling back to `workflows::types::DEFAULT_ALLOWED_TOOLS`
    /// using the workflow's default allowed tools when the step omits them.
    pub allowed_tools: Vec<String>,
    /// From `step.bash_allowlist`, verbatim (empty when unset).
    pub bash_allowlist: Vec<String>,
    /// From `RunRecord.system_prompt`, followed by the backend-neutral task-control contract.
    /// Skill and handoff instructions are assembled by the caller that owns those features.
    pub system_prompt: Option<String>,
    /// From `RunRecord.reasoning_effort`, mapped from the `auto`-inclusive contract enum to the
    /// concrete one a backend spawn actually takes (`Auto` becomes `None`, letting the backend
    /// use its own default).
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
    pub cancellation: CancellationToken,
}

/// Usage and marker information a fake or a future backend mapper can report without leaking its
/// native event types into core.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionReport {
    pub session_id: Option<String>,
    pub tokens_used: f64,
    pub input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub cost_usd: Option<f64>,
    pub turn_text: String,
    pub decision: Option<TurnMarkerDecision>,
    pub plan_entries: Option<Vec<context_refresh::PlanEntry>>,
}

/// A single injected session turn. `Running` models a session that still owns a parallel slot;
/// `Waiting` models an open session parked for a user/monitoring turn and therefore releases it.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOutcome {
    Completed(SessionReport),
    Running(SessionReport),
    Waiting(SessionReport),
    Failed {
        message: String,
        report: SessionReport,
    },
    Cancelled(SessionReport),
}

/// Backend-neutral session seam. A real runner adapter belongs outside this crate and only needs
/// to translate its own protocol into these outcomes.
pub trait AgentSession: Send {
    /// Run one turn. `on_event` is called once per mid-turn live event, in order, before this
    /// returns — a real backend calls it as its process actually produces output; a fake/test
    /// double may call it zero or more times, or not at all, and still return a valid outcome.
    /// The returned [`SessionOutcome`]'s [`SessionReport::turn_text`] is the whole turn's
    /// aggregated text, used for post-turn bookkeeping (marker detection, titles) — it is not
    /// re-persisted as its own event; `on_event` already carried the content live.
    fn turn(
        &mut self,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String>;

    fn send_message(
        &mut self,
        _prompt: &str,
        _images: &[PromptImage],
        _on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        Err("session does not accept follow-up messages".to_owned())
    }

    fn finish(
        &mut self,
        _on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<SessionOutcome, String> {
        Ok(SessionOutcome::Completed(SessionReport::default()))
    }

    fn cancel(&mut self) {}

    fn session_id(&self) -> Option<String> {
        None
    }
}

/// Factory seam for session creation. It is injected by the CLI/engine integration layer or by a
/// deterministic test fake; no backend-specific runner type crosses this boundary.
pub trait SessionFactory: Send + Sync {
    fn open(&self, request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String>;

    fn request_cancel(&self, _run_id: &str) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub success: bool,
    pub exit_code: i32,
    pub output: String,
}

/// Check execution is injected for the same reason as sessions: core owns workflow semantics, not
/// a shell/process policy.
pub trait CheckExecutor: Send {
    fn run(&mut self, command: &str, cwd: &Path) -> Result<CheckResult, String>;
}

/// Review settlement asks an injected diff reader whether the run has changes. This keeps Git
/// worktree I/O out of the runtime foundation while preserving the review decision.
pub trait DiffInspector: Send {
    fn has_diff(&mut self, run: &RunRecord) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub max_parallel: usize,
    pub max_monitoring_sessions: usize,
    /// A durable deadline for a parked monitoring turn. `None` deliberately disables timer
    /// driven wake-up; callers can still deliver an explicit monitoring message.
    pub monitoring_wake_interval_minutes: Option<u64>,
    pub review_gate: bool,
    pub auto_resume_on_usage_limit: bool,
}

/// Sanitized local counters for diagnostics and scaling tests. They intentionally contain no
/// prompt, credential, provider-payload, or transcript data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    pub event_appends: usize,
    pub index_flushes: usize,
    /// Cumulative bytes in successfully written index snapshots. This is local diagnostic
    /// accounting only; it never includes event payloads or provider output.
    pub index_flush_bytes: u64,
    pub active_sessions: usize,
    pub queued_jobs: usize,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            max_parallel: 2,
            max_monitoring_sessions: 2,
            monitoring_wake_interval_minutes: None,
            review_gate: false,
            auto_resume_on_usage_limit: true,
        }
    }
}

enum RuntimeJob {
    Workflow {
        workflow: WorkflowDef,
        start_index: usize,
        retry_counts: BTreeMap<String, u32>,
    },
    Continuation {
        workflow: WorkflowDef,
        step_index: usize,
        session_id: Option<String>,
        prompt: String,
        images: Vec<PromptImage>,
        runner: RunnerSelection,
        model: Option<String>,
        retry_counts: BTreeMap<String, u32>,
    },
}

/// Failover eligibility scoped to one live turn's original open attempt: `try_auto_failover`
/// only ever applies to the fresh session that hit the failure, never to a later externally
/// resumed interaction. [`RuntimeActive::park`] drops this the moment a turn goes idle so a
/// `deliver_message`/`finish` resume can never inherit it.
#[derive(Debug, Clone)]
struct FailoverContext {
    concrete: Runner,
    retry_prompt: String,
}

/// A live turn moved out of the manager's lock for the duration of its blocking session I/O.
/// The session itself lives on a per-run worker; this struct is the opaque handle threaded back
/// into the manager (via [`RunManager::apply_admitted_turn`]/[`RunManager::apply_active_turn`])
/// to apply each streamed event and terminal outcome under a briefly held lock. Its fields stay
/// private — a caller in another crate can hold and move the value but never construct or
/// inspect it directly.
pub struct RuntimeActive {
    workflow: WorkflowDef,
    step_index: usize,
    next_index: usize,
    retry_counts: BTreeMap<String, u32>,
    session: Box<dyn AgentSession + Send>,
    holds_slot: bool,
    plan_checkpoint: context_refresh::PlanCheckpoint,
    auto_continues: u32,
    failover: Option<FailoverContext>,
}

impl RuntimeActive {
    /// The only way an external worker touches the live session: call `turn`/`send_message` on
    /// it directly (never through the manager lock), then hand the same [`RuntimeActive`] back.
    pub fn session_mut(&mut self) -> &mut (dyn AgentSession + Send) {
        self.session.as_mut()
    }

    /// The step this turn belongs to — a caller sending a message against it (an autonomous
    /// nudge, a monitoring wake) needs this to tag durable events with the same step the manager
    /// itself will look for when it applies the result.
    pub fn step_id(&self) -> &str {
        &self.workflow.steps[self.step_index].id
    }
}

/// Everything [`RunManager::execute_job`] has already computed for a step by the time it needs a
/// live session — captured so the caller can open the session and run the turn outside the
/// manager's lock, then resume exactly where the workflow loop left off.
struct PendingResume {
    workflow: WorkflowDef,
    index: usize,
    retry_counts: BTreeMap<String, u32>,
    plan_checkpoint: context_refresh::PlanCheckpoint,
    concrete: Runner,
    retry_prompt: String,
}

/// A turn admitted for execution: the request is ready to open, but opening and running it is
/// the caller's job, entirely outside the manager's lock. Pass the result back through
/// [`RunManager::apply_open_failure`] (open failed) or [`RunManager::apply_admitted_turn`] (open
/// and the first turn both ran). Opaque outside this module beyond the two `pub` fields a caller
/// needs to actually open the session.
pub struct AdmittedTurn {
    pub run_id: String,
    pub step_id: String,
    pub request: SessionRequest,
    resume: PendingResume,
}

/// What a caller driving a live turn must do next.
pub enum TurnStep {
    /// This worker's dispatch is finished — terminal state (parked, failed, cancelled, completed,
    /// or requeued) has already been applied durably.
    Done,
    /// The run is autonomous and the manager decided to nudge it. The caller must call
    /// `active.session_mut().send_message(..)` (no lock held) and report the result back through
    /// [`RunManager::apply_active_turn`].
    Nudge(Box<RuntimeActive>),
    /// Ask the completed session for the one-line subject used by the production dispatcher to
    /// commit and push this run's changes.
    GitAutoCommit(Box<RuntimeActive>),
}

/// The lock-held half of a user-requested finish. Runs without a process-local session can be
/// settled immediately; a parked live session is detached so its blocking `finish` call happens
/// on the production dispatcher and is later applied through [`RunManager::apply_finish_turn`].
pub enum FinishStart {
    Finished(bool),
    Detached(Box<RuntimeActive>),
}

/// Result of the synthetic automatic-commit subject turn.
pub enum GitAutoMessage {
    Subject(String),
    Cancelled,
}

/// A stateful, synchronous facade over the durable run files.
pub struct RunManager {
    data_dir: PathBuf,
    repo_root: Option<PathBuf>,
    runs: BTreeMap<String, RunRecord>,
    seqs: HashMap<String, f64>,
    queue: QueueState,
    usage: HashMap<String, UsageInvocation>,
    next_observer_id: u64,
    event_observers: EventObservers,
    run_observers: RunObservers,
    session_factory: Option<Box<dyn SessionFactory>>,
    check_executor: Option<Box<dyn CheckExecutor>>,
    diff_inspector: Option<Box<dyn DiffInspector>>,
    runtime_options: RuntimeOptions,
    jobs: BTreeMap<String, RuntimeJob>,
    active: BTreeMap<String, RuntimeActive>,
    /// Turns `execute_job` admitted but has not yet handed to a worker. Counted as busy by
    /// `runtime_busy_slots` from the moment they land here until `apply_open_failure` or
    /// `apply_admitted_turn`/`apply_active_turn` resolves them.
    pending_turns: VecDeque<AdmittedTurn>,
    in_flight: BTreeSet<String>,
    project_id: String,
    workspace_semaphore: Option<Box<dyn WorkspaceSemaphore>>,
    repository_lease: Option<Box<dyn RepositoryRootLease>>,
    workspace_holds: BTreeSet<String>,
    repository_holds: BTreeSet<String>,
    plan_checkpoints: BTreeMap<String, context_refresh::PlanCheckpoint>,
    pending_context_prompts: BTreeMap<String, String>,
    auto_routes: BTreeMap<String, Vec<Runner>>,
    intelligent_context_refresh: bool,
    last_index_flush: Instant,
    write_quarantined: bool,
    index_write_count: usize,
    index_write_bytes: u64,
    event_append_count: usize,
    event_appenders: HashMap<String, events::BufferedEventAppender>,
}

impl RunManager {
    /// Open a live manager. Active records are retained so callers can reconcile or resume them.
    pub fn open(data_dir: impl Into<PathBuf>) -> Self {
        Self::open_with_keep_live(data_dir, true)
    }

    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::open(data_dir)
    }

    pub fn with_session_factory(
        data_dir: impl Into<PathBuf>,
        session_factory: impl SessionFactory + 'static,
    ) -> Self {
        let mut manager = Self::open(data_dir);
        manager.session_factory = Some(Box::new(session_factory));
        manager
    }

    /// Construct a manager whose durable state is outside its repository. The repository root
    /// is retained explicitly instead of being inferred from the state-directory layout.
    pub fn with_session_factory_for_repo(
        repo_root: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        session_factory: impl SessionFactory + 'static,
    ) -> Self {
        let mut manager = Self::with_session_factory(data_dir, session_factory);
        manager.repo_root = Some(repo_root.into());
        manager
    }

    pub fn open_with_keep_live(data_dir: impl Into<PathBuf>, keep_live: bool) -> Self {
        let data_dir = data_dir.into();
        let _ = fs::create_dir_all(data_dir.join("runs"));
        let index_path = store::index_path(&data_dir);
        let load = store::load_run_index_outcome(&index_path, keep_live);
        let write_quarantined = load.write_quarantined();
        if write_quarantined {
            eprintln!(
                "coducktor: {} contains corrupt run state; preserving it and quarantining writes",
                index_path.display()
            );
        }
        let loaded = load.records().to_vec();
        let runs = loaded
            .into_iter()
            .map(|run| (run.id.clone(), run))
            .collect();
        Self {
            data_dir,
            repo_root: None,
            runs,
            seqs: HashMap::new(),
            queue: QueueState::default(),
            usage: HashMap::new(),
            next_observer_id: 0,
            event_observers: BTreeMap::new(),
            run_observers: BTreeMap::new(),
            session_factory: None,
            check_executor: None,
            diff_inspector: None,
            runtime_options: RuntimeOptions::default(),
            jobs: BTreeMap::new(),
            active: BTreeMap::new(),
            pending_turns: VecDeque::new(),
            in_flight: BTreeSet::new(),
            project_id: "default".to_owned(),
            workspace_semaphore: None,
            repository_lease: None,
            workspace_holds: BTreeSet::new(),
            repository_holds: BTreeSet::new(),
            plan_checkpoints: BTreeMap::new(),
            pending_context_prompts: BTreeMap::new(),
            auto_routes: BTreeMap::new(),
            intelligent_context_refresh: false,
            last_index_flush: Instant::now(),
            write_quarantined,
            index_write_count: 0,
            index_write_bytes: 0,
            event_append_count: 0,
            event_appenders: HashMap::new(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Repository root a session should run in. Managers created by core-only tests can omit it;
    /// production construction always provides it explicitly.
    fn repo_root(&self) -> PathBuf {
        self.repo_root
            .clone()
            .unwrap_or_else(|| self.data_dir.clone())
    }

    pub fn set_session_factory(&mut self, session_factory: impl SessionFactory + 'static) {
        self.session_factory = Some(Box::new(session_factory));
    }

    pub fn set_check_executor(&mut self, check_executor: impl CheckExecutor + 'static) {
        self.check_executor = Some(Box::new(check_executor));
    }

    pub fn set_diff_inspector(&mut self, diff_inspector: impl DiffInspector + 'static) {
        self.diff_inspector = Some(Box::new(diff_inspector));
    }

    pub fn set_runtime_options(&mut self, options: RuntimeOptions) {
        self.runtime_options = options;
    }

    pub fn set_project_id(&mut self, project_id: impl Into<String>) {
        self.project_id = project_id.into();
    }

    pub fn set_workspace_semaphore(&mut self, semaphore: impl WorkspaceSemaphore + 'static) {
        self.workspace_semaphore = Some(Box::new(semaphore));
    }

    pub fn set_repository_lease(&mut self, lease: impl RepositoryRootLease + 'static) {
        self.repository_lease = Some(Box::new(lease));
    }

    pub fn set_intelligent_context_refresh(&mut self, enabled: bool) {
        self.intelligent_context_refresh = enabled;
    }

    pub fn runtime_options(&self) -> RuntimeOptions {
        self.runtime_options
    }

    pub fn runtime_metrics(&self) -> RuntimeMetrics {
        RuntimeMetrics {
            event_appends: self.event_append_count,
            index_flushes: self.index_write_count,
            index_flush_bytes: self.index_write_bytes,
            active_sessions: self.active.len(),
            queued_jobs: self.jobs.len(),
        }
    }

    pub fn get_run(&self, run_id: &str) -> Option<&RunRecord> {
        self.runs.get(run_id)
    }

    pub fn list_runs(&self) -> Vec<RunRecord> {
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        store::list_runs_by_recency(&records)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Remove a run and its append-only event log. Callers must check that the run is not active.
    pub fn remove_run(&mut self, run_id: &str) -> io::Result<bool> {
        let Some(run) = self.runs.remove(run_id) else {
            return Ok(false);
        };
        self.cleanup_runtime(run_id);
        if let Err(error) = self.persist() {
            self.runs.insert(run_id.to_owned(), run);
            return Err(error);
        }
        let event_path = events::events_path(&self.data_dir, run_id);
        let _ = fs::remove_file(event_path);
        crate::handoff::delete_handoff(&self.data_dir, run_id);
        Ok(true)
    }

    /// Remove terminal records beyond the durable index retention budget. The replacement index
    /// is committed before best-effort sidecar cleanup, so a crash cannot leave an index entry
    /// that points at already-deleted history. Queued, running, and waiting work is never a
    /// retention candidate, even if an old clock or imported state makes it look stale.
    ///
    /// Worktrees have their own explicit retention policy: removing an index record must not
    /// remove a checkout that may contain recoverable agent edits.
    pub fn prune_stale_runs(&mut self) -> io::Result<Vec<String>> {
        let candidates = store::select_stale_run_ids(&self.list_runs());
        let stale: Vec<String> = candidates
            .into_iter()
            .filter(|run_id| {
                self.runs.get(run_id).is_some_and(|run| {
                    matches!(
                        run.status,
                        RunStatus::Done
                            | RunStatus::Failed
                            | RunStatus::Cancelled
                            | RunStatus::Review
                    ) && !self.is_active(run_id)
                })
            })
            .collect();
        if stale.is_empty() {
            return Ok(stale);
        }

        let removed: Vec<(String, RunRecord)> = stale
            .iter()
            .filter_map(|run_id| self.runs.remove(run_id).map(|run| (run_id.clone(), run)))
            .collect();
        if let Err(error) = self.persist() {
            self.runs.extend(removed);
            return Err(error);
        }
        for run_id in &stale {
            let _ = fs::remove_file(events::events_path(&self.data_dir, run_id));
            crate::handoff::delete_handoff(&self.data_dir, run_id);
        }
        Ok(stale)
    }

    /// Register an observer for appended events. The callback is invoked after the NDJSON append
    /// succeeds and receives an owned notification view, so it cannot mutate manager state by
    /// aliasing a record reference.
    pub fn subscribe_events<F>(&mut self, observer: F) -> EventObserverId
    where
        F: Fn(&RunEventNotification) + Send + Sync + 'static,
    {
        let id = self.next_observer_id();
        self.event_observers.insert(id, Box::new(observer));
        id
    }

    pub fn unsubscribe_events(&mut self, observer_id: EventObserverId) -> bool {
        self.event_observers.remove(&observer_id).is_some()
    }

    /// Register an observer for durable record updates.
    pub fn subscribe_runs<F>(&mut self, observer: F) -> RunObserverId
    where
        F: Fn(&RunRecord) + Send + Sync + 'static,
    {
        let id = self.next_observer_id();
        self.run_observers.insert(id, Box::new(observer));
        id
    }

    pub fn unsubscribe_runs(&mut self, observer_id: RunObserverId) -> bool {
        self.run_observers.remove(&observer_id).is_some()
    }

    fn next_observer_id(&mut self) -> u64 {
        self.next_observer_id = self.next_observer_id.wrapping_add(1);
        self.next_observer_id
    }

    fn persist(&mut self) -> io::Result<()> {
        if self.write_quarantined {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runs.json is quarantined because existing state could not be fully loaded",
            ));
        }
        fs::create_dir_all(self.data_dir.join("runs"))?;
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        let index_path = store::index_path(&self.data_dir);
        store::write_run_index(&index_path, &records)?;
        self.index_write_count += 1;
        if let Ok(metadata) = fs::metadata(index_path) {
            self.index_write_bytes = self.index_write_bytes.saturating_add(metadata.len());
        }
        self.last_index_flush = Instant::now();
        Ok(())
    }

    /// Flush is kept explicit for callers that want a named shutdown boundary. Mutations are
    /// already written synchronously before they return.
    pub fn flush(&mut self) -> io::Result<()> {
        self.persist()
    }

    /// Repair state that was quarantined during load. This is deliberately explicit: the
    /// original index is backed up before the currently salvaged records replace it.
    pub fn repair_quarantined_index(&mut self) -> io::Result<Option<PathBuf>> {
        if !self.write_quarantined {
            return Ok(None);
        }
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        let backup =
            store::backup_then_repair_run_index(&store::index_path(&self.data_dir), &records)?;
        self.write_quarantined = false;
        self.last_index_flush = Instant::now();
        Ok(Some(backup))
    }

    fn notify_run(&self, run: &RunRecord) {
        for observer in self.run_observers.values() {
            observer(run);
        }
    }

    fn notify_event(&self, notification: &RunEventNotification) {
        for observer in self.event_observers.values() {
            observer(notification);
        }
    }

    fn replace_record(
        &mut self,
        run_id: &str,
        previous: RunRecord,
        next: RunRecord,
    ) -> io::Result<RunRecord> {
        self.runs.insert(run_id.to_owned(), next.clone());
        if let Err(error) = self.persist() {
            self.runs.insert(run_id.to_owned(), previous);
            return Err(error);
        }
        self.notify_run(&next);
        Ok(next)
    }

    /// Create and durably persist a queued record.
    pub fn create_run(&mut self, input: CreateRunInput) -> io::Result<RunRecord> {
        let id = new_run_id();
        let created_at = now_iso8601();
        let mut steps: Vec<StepState> = input.steps.into_iter().map(step_from_seed).collect();
        if let (Some(decision), Some(first)) = (input.routing_decision, steps.first_mut()) {
            first.routing_decision = Some(decision);
        }
        let run = RunRecord {
            id: id.clone(),
            title: input.title,
            workflow: input.workflow,
            task: input.task,
            task_images: input.task_images,
            model: input.model,
            reasoning_effort: input.reasoning_effort,
            model_identity: input.model_identity,
            runner: input.runner,
            requested_runner: input.requested_runner,
            agent_profile: input.agent_profile,
            system_prompt: input.system_prompt,
            autonomous: input.autonomous,
            git_auto: input.git_auto,
            worktree: input.worktree,
            group_id: input.group_id,
            variant: input.variant,
            status: RunStatus::Queued,
            created_at: created_at.clone(),
            updated_at: Some(created_at),
            tokens_used: 0.0,
            archived: false,
            steps,
            workflow_def: input.workflow_def,
            ..RunRecord::default()
        };
        self.runs.insert(id.clone(), run.clone());
        if let Err(error) = self.persist() {
            self.runs.remove(&id);
            return Err(error);
        }
        self.notify_run(&run);
        Ok(run)
    }

    pub fn create_workflow_run(
        &mut self,
        workflow: &WorkflowDef,
        task: impl Into<String>,
    ) -> io::Result<RunRecord> {
        self.create_run(CreateRunInput::from_workflow(workflow, task))
    }

    /// Create and queue one workflow without running it.
    ///
    /// Interactive clients use this accepted-first boundary to obtain the durable run id and
    /// subscribe to its event stream before a potentially long-running agent turn begins.
    pub fn enqueue_run(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
    ) -> io::Result<RunRecord> {
        let mut create = CreateRunInput::from_workflow(workflow, input.task.clone());
        create.model = input.model;
        create.reasoning_effort = input.reasoning_effort;
        create.runner = input
            .resolved_runner
            .or_else(|| input.runner.and_then(concrete_runner));
        create.requested_runner = input.runner;
        create.agent_profile = input.agent_profile;
        create.system_prompt = input.system_prompt;
        create.autonomous = input.autonomous;
        create.git_auto = input.git_auto;
        create.worktree = input.worktree;
        create.task_images = (!input.images.is_empty())
            .then(|| input.images.iter().map(PromptImage::data_url).collect());
        create.routing_decision = input.routing_decision;
        let run = self.create_run(create)?;
        if let Some(decision) = run
            .steps
            .first()
            .and_then(|step| step.routing_decision.as_ref())
        {
            self.append_event(
                &run.id,
                EventInput::new("note").field("message", routing_decision_note(decision)),
            )?;
            self.append_event(
                &run.id,
                EventInput::new("routing-decision").field("decision", decision),
            )?;
        }
        if input.runner == Some(RunnerSelection::Auto) {
            self.auto_routes
                .insert(run.id.clone(), input.auto_runner_candidates);
        }
        self.jobs.insert(
            run.id.clone(),
            RuntimeJob::Workflow {
                workflow: workflow.clone(),
                start_index: 0,
                retry_counts: BTreeMap::new(),
            },
        );
        self.enqueue(run.id.clone());
        Ok(run)
    }

    /// Create, queue, and pump one workflow. The returned record is the current durable state,
    /// which may already be terminal when the injected session completes synchronously.
    pub fn start_run(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
    ) -> io::Result<RunRecord> {
        let run = self.enqueue_run(workflow, input)?;
        self.run_to_completion()?;
        Ok(self.get_run(&run.id).cloned().unwrap_or(run))
    }

    /// Start up to the three built-in variants in one queue pass. Variant B/C receive the same
    /// fixed diversification hints while the runtime still treats them as ordinary queued jobs.
    pub fn enqueue_variants(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
        count: usize,
    ) -> io::Result<Vec<RunRecord>> {
        let group_id = new_run_id();
        let metadata = variants::variant_metadata(&group_id, &input.task, count);
        let mut ids = Vec::with_capacity(count);
        for variant in metadata {
            let mut variant_input = input.clone();
            variant_input.task = variant.task;
            let mut create = CreateRunInput::from_workflow(workflow, variant_input.task.clone());
            create.title = format!("{} ({})", create.title, variant.variant);
            create.model = variant_input.model;
            create.reasoning_effort = variant_input.reasoning_effort;
            create.runner = variant_input
                .resolved_runner
                .or_else(|| variant_input.runner.and_then(concrete_runner));
            create.requested_runner = variant_input.runner;
            create.agent_profile = variant_input.agent_profile;
            create.system_prompt = variant_input.system_prompt;
            create.autonomous = variant_input.autonomous;
            create.worktree = (!variant.isolated).then_some(false);
            create.group_id = Some(group_id.clone());
            create.variant = Some(variant.variant);
            create.task_images = (!variant_input.images.is_empty()).then(|| {
                variant_input
                    .images
                    .iter()
                    .map(PromptImage::data_url)
                    .collect()
            });
            let run = self.create_run(create)?;
            if variant_input.runner == Some(RunnerSelection::Auto) {
                self.auto_routes
                    .insert(run.id.clone(), variant_input.auto_runner_candidates);
            }
            ids.push(run.id.clone());
            self.jobs.insert(
                run.id.clone(),
                RuntimeJob::Workflow {
                    workflow: workflow.clone(),
                    start_index: 0,
                    retry_counts: BTreeMap::new(),
                },
            );
            self.enqueue(run.id);
        }
        Ok(ids
            .into_iter()
            .filter_map(|run_id| self.get_run(&run_id).cloned())
            .collect())
    }

    pub fn start_variants(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
        count: usize,
    ) -> io::Result<Vec<RunRecord>> {
        let runs = self.enqueue_variants(workflow, input, count)?;
        self.run_to_completion()?;
        Ok(runs
            .into_iter()
            .map(|run| self.get_run(&run.id).cloned().unwrap_or(run))
            .collect())
    }

    /// Apply a durable contract-shaped patch. Unknown keys are ignored by the shared serde
    /// contract, while wrong values fail before the old record is replaced.
    pub fn update_run(&mut self, run_id: &str, patch: RunPatch) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = apply_run_patch(&previous, patch.fields())?;
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn update_run_value(
        &mut self,
        run_id: &str,
        patch: Value,
    ) -> io::Result<Option<RunRecord>> {
        let patch = RunPatch::from_value(patch)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.update_run(run_id, patch)
    }

    /// Escape hatch for lifecycle code that already has a typed `RunRecord` mutation. The
    /// resulting value still goes through the same durable replace/rollback path.
    pub fn edit_run<F>(&mut self, run_id: &str, edit: F) -> io::Result<Option<RunRecord>>
    where
        F: FnOnce(&mut RunRecord),
    {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        edit(&mut next);
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    /// Append a step once. Duplicate ids are a no-op, matching `RunStore.addStep`.
    pub fn add_step(&mut self, run_id: &str, step: StepSeed) -> io::Result<bool> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        if previous
            .steps
            .iter()
            .any(|candidate| candidate.id == step.id)
        {
            return Ok(false);
        }
        let mut next = previous.clone();
        next.steps.push(step_from_seed(step));
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    /// Update a step and recompute all run-level usage and cost aggregates.
    pub fn update_step(
        &mut self,
        run_id: &str,
        step_id: &str,
        patch: StepPatch,
    ) -> io::Result<bool> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step_index) = previous.steps.iter().position(|step| step.id == step_id) else {
            return Ok(false);
        };
        let mut next = previous.clone();
        apply_step_patch(&mut next, step_index, patch.fields())?;
        recompute_aggregates(&mut next);
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    pub fn edit_step<F>(&mut self, run_id: &str, step_id: &str, edit: F) -> io::Result<bool>
    where
        F: FnOnce(&mut StepState),
    {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step_index) = previous.steps.iter().position(|step| step.id == step_id) else {
            return Ok(false);
        };
        let mut next = previous.clone();
        edit(&mut next.steps[step_index]);
        recompute_aggregates(&mut next);
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    /// Persist the invocation checkpoint before a backend session is launched.
    pub fn begin_usage_invocation(&mut self, run_id: &str, step_id: &str) -> io::Result<bool> {
        let Some(step) = self
            .runs
            .get(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == step_id))
        else {
            return Ok(false);
        };
        let epoch = step.usage_invocation_epoch.unwrap_or(0.0) + 1.0;
        let patch = RunPatch::new().set("usageInvocationEpoch", epoch).set(
            "usageInvocationsStarted",
            step.usage_invocations_started.unwrap_or(0.0) + 1.0,
        );
        if !self.update_step(run_id, step_id, patch)? {
            return Ok(false);
        }
        self.usage.insert(
            run_id.to_owned(),
            UsageInvocation {
                step_id: step_id.to_owned(),
                observed: false,
                started_turns: HashSet::new(),
                recorded_turns: HashSet::new(),
            },
        );
        Ok(true)
    }

    /// Record a backend-neutral turn checkpoint, deduplicated inside the current invocation.
    pub fn record_usage_event(&mut self, run_id: &str, event: UsageEvent) -> io::Result<bool> {
        let Some(invocation) = self.usage.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step) = self
            .runs
            .get(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == invocation.step_id))
        else {
            return Ok(false);
        };
        match event {
            UsageEvent::TurnStarted { turn_id } => {
                if invocation.started_turns.contains(&turn_id) {
                    return Ok(false);
                }
                let first_observed = !invocation.observed;
                let mut patch = RunPatch::new().set(
                    "usageTurnsStarted",
                    step.usage_turns_started.unwrap_or(0.0) + 1.0,
                );
                if first_observed {
                    patch = patch.set(
                        "usageInvocationsObserved",
                        step.usage_invocations_observed.unwrap_or(0.0) + 1.0,
                    );
                }
                if !self.update_step(run_id, &invocation.step_id, patch)? {
                    return Ok(false);
                }
                if let Some(current) = self.usage.get_mut(run_id) {
                    current.started_turns.insert(turn_id);
                    current.observed = true;
                }
                Ok(true)
            }
            UsageEvent::TurnCompleted {
                turn_id,
                input_tokens,
                output_tokens,
            } => {
                let Some(input_tokens) = input_tokens else {
                    return Ok(false);
                };
                let Some(output_tokens) = output_tokens else {
                    return Ok(false);
                };
                if !input_tokens.is_finite()
                    || input_tokens < 0.0
                    || !output_tokens.is_finite()
                    || output_tokens < 0.0
                    || !invocation.started_turns.contains(&turn_id)
                    || invocation.recorded_turns.contains(&turn_id)
                {
                    return Ok(false);
                }
                let patch = RunPatch::new()
                    .set(
                        "inputTokens",
                        step.input_tokens.unwrap_or(0.0) + input_tokens,
                    )
                    .set(
                        "outputTokens",
                        step.output_tokens.unwrap_or(0.0) + output_tokens,
                    )
                    .set(
                        "usageTurnsRecorded",
                        step.usage_turns_recorded.unwrap_or(0.0) + 1.0,
                    );
                if !self.update_step(run_id, &invocation.step_id, patch)? {
                    return Ok(false);
                }
                if let Some(current) = self.usage.get_mut(run_id) {
                    current.recorded_turns.insert(turn_id);
                }
                Ok(true)
            }
        }
    }

    /// Append one event with a manager-owned sequence and timestamp.
    pub fn append_event(&mut self, run_id: &str, input: EventInput) -> io::Result<RunEvent> {
        if !self.runs.contains_key(run_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown run: {run_id}"),
            ));
        }
        let path = events::events_path(&self.data_dir, run_id);
        let seq = self
            .seqs
            .get(run_id)
            .copied()
            .unwrap_or_else(|| events::rehydrate_seq(&path))
            + 1.0;
        let event = RunEvent {
            seq,
            ts: now_iso8601(),
            step_id: input.step_id,
            event_type: input.event_type,
            extra: input.extra,
        };
        if !self.event_appenders.contains_key(run_id) {
            self.event_appenders.insert(
                run_id.to_owned(),
                events::BufferedEventAppender::open(&path)?,
            );
        }
        self.event_appenders
            .get_mut(run_id)
            .ok_or_else(|| io::Error::other("event appender unavailable"))?
            .append(&event)?;
        self.event_append_count += 1;
        // Event append is meaningful activity. Keep read/unread and archive mutations on their
        // separate timestamps by stamping here instead of in the generic record replacement.
        let updated_run = if let Some(run) = self.runs.get_mut(run_id) {
            run.updated_at = Some(event.ts.clone());
            Some(run.clone())
        } else {
            None
        };
        let flush_index = self.last_index_flush.elapsed() >= Duration::from_millis(250);
        if updated_run.is_some() && flush_index {
            self.persist()?;
        }
        self.seqs.insert(run_id.to_owned(), seq);
        if flush_index && let Some(run) = &updated_run {
            self.notify_run(run);
        }
        let notification = RunEventNotification {
            run_id: run_id.to_owned(),
            event: event.clone(),
        };
        self.notify_event(&notification);
        Ok(event)
    }

    pub fn append_event_fields(
        &mut self,
        run_id: &str,
        event_type: impl Into<String>,
        step_id: Option<String>,
        extra: Map<String, Value>,
    ) -> io::Result<RunEvent> {
        self.append_event(
            run_id,
            EventInput {
                event_type: event_type.into(),
                step_id,
                extra,
            },
        )
    }

    /// A per-turn live event sink: a real [`AgentSession`] calls this once per mid-turn event
    /// (each raw process message a backend's UI mapper turns into one, e.g. a `message.updated`
    /// or `tool.call` chunk) so the transcript persists — and broadcasts to live subscribers —
    /// as the agent actually produces it, rather than waiting for the whole turn to finish. This
    /// is what makes `session.turn()`'s event-sink parameter meaningful: without it, only the
    /// aggregated [`SessionReport::turn_text`] would exist, once, after the turn already ended.
    ///
    /// A `text`-typed event is marker-stripped per chunk
    /// (so `DUCK:DONE`/`DUCK:MONITORING`/task markers never flash in the live transcript, matching
    /// [`Self::apply_session_markers`]'s aggregate-side detection) and dropped if that empties it;
    /// every other event type passes through unchanged.
    fn event_sink(
        &mut self,
        run_id: &str,
        step_id: &str,
    ) -> impl FnMut(EventInput) -> io::Result<()> + '_ {
        let run_id = run_id.to_owned();
        let step_id = step_id.to_owned();
        move |mut event: EventInput| {
            if event.step_id.is_none() {
                event.step_id = Some(step_id.clone());
            }
            if event.event_type == "text"
                && let Some(text) = event.extra.get("text").and_then(Value::as_str)
            {
                let stripped = strip_turn_marker(&task_markers::strip_task_markers(text));
                if stripped.is_empty() {
                    return Ok(());
                }
                event
                    .extra
                    .insert("text".to_owned(), Value::String(stripped));
            }
            self.append_event(&run_id, event).map(|_| ())
        }
    }

    /// Apply one live event a worker's `AgentSession::turn`/`send_message` produced, outside any
    /// lock, to durable state. A caller holds this manager's lock only for the duration of this
    /// one call, in between reads from the (lock-free) child process — never across the I/O
    /// itself. Same normalization as the synchronous `event_sink` path.
    pub fn apply_turn_event(
        &mut self,
        run_id: &str,
        step_id: &str,
        event: EventInput,
    ) -> io::Result<()> {
        (self.event_sink(run_id, step_id))(event)
    }

    /// Read the raw event history through the shared event reader.
    pub fn read_events(&self, run_id: &str) -> Vec<RunEvent> {
        events::read_events(&events::events_path(&self.data_dir, run_id))
    }

    pub fn set_archived(&mut self, run_id: &str, archived: bool) -> io::Result<Option<RunRecord>> {
        let now = now_iso8601();
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.archived = archived;
        next.archived_at = archived.then_some(now);
        if archived {
            next.auto_resume_at = None;
            next.auto_resume_attempts = None;
        }
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn archive(&mut self, run_id: &str, archived: bool) -> io::Result<Option<RunRecord>> {
        self.set_archived(run_id, archived)
    }

    /// Archive every terminal run in one durable write and return the number changed.
    pub fn archive_finished(&mut self) -> io::Result<usize> {
        let now = now_iso8601();
        let previous = self.runs.clone();
        let mut changed = Vec::new();
        for run in self.runs.values_mut() {
            if !run.archived
                && matches!(
                    run.status,
                    RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
                )
            {
                run.archived = true;
                run.archived_at = Some(now.clone());
                run.auto_resume_at = None;
                run.auto_resume_attempts = None;
                changed.push(run.clone());
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.persist() {
            self.runs = previous;
            return Err(error);
        }
        for run in &changed {
            self.notify_run(run);
        }
        Ok(changed.len())
    }

    pub fn set_read(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.seen_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn mark_read(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        self.set_read(run_id)
    }

    pub fn set_unread(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.seen_at = None;
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn mark_unread(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        self.set_unread(run_id)
    }

    /// Stamp currently unread finished runs and return the number stamped.
    pub fn mark_all_read(&mut self) -> io::Result<usize> {
        let now = now_iso8601();
        let previous = self.runs.clone();
        let mut changed = Vec::new();
        for run in self.runs.values_mut() {
            if is_unread(run) {
                run.seen_at = Some(now.clone());
                changed.push(run.clone());
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.persist() {
            self.runs = previous;
            return Err(error);
        }
        for run in &changed {
            self.notify_run(run);
        }
        Ok(changed.len())
    }

    /// Add an id to the in-memory FIFO. The run itself is already durably queued by creation or
    /// lifecycle code; queue membership is intentionally process-local.
    pub fn enqueue(&mut self, run_id: impl Into<String>) -> bool {
        self.queue.enqueue(run_id)
    }

    pub fn take_next(&mut self) -> Option<String> {
        self.queue.take_next()
    }

    pub fn finish_start(&mut self, run_id: &str) -> bool {
        self.queue.finish_start(run_id)
    }

    pub fn queue(&self) -> &QueueState {
        &self.queue
    }

    pub fn is_active(&self, run_id: &str) -> bool {
        self.active.contains_key(run_id)
            || self.jobs.contains_key(run_id)
            || self.queue.is_queued(run_id)
            || self.queue.is_starting(run_id)
    }

    pub fn recover_queued(&mut self) -> Vec<String> {
        self.queue = QueueState::default();
        for run_id in fifo_run_ids(&self.list_runs()) {
            self.queue.enqueue(run_id);
        }
        self.queue.queued().map(str::to_owned).collect()
    }

    pub fn hydrate_queued_prompt(&self, run_id: &str) -> Option<String> {
        self.get_run(run_id).map(hydrate_queued_prompt)
    }

    pub fn account_holds(&self) -> AccountHolds {
        let now = now_iso8601();
        derive_account_holds(&self.list_runs(), &now)
    }

    pub fn account_holds_at(&self, now: &str) -> AccountHolds {
        derive_account_holds(&self.list_runs(), now)
    }

    pub fn reconcile_quota_at(&self, now: &str) -> QuotaReconciliation {
        reconcile_quota(&self.list_runs(), now, Runner::Claude)
    }

    /// Apply one deterministic auto-resume pass. Future deadlines remain durable holds; only due
    /// runs whose account is not held are handed to the ordinary continuation queue.
    pub fn reconcile_auto_resumes(&mut self, now: &str) -> io::Result<AutoResumeReport> {
        let plan = self.reconcile_quota_at(now);
        let mut report = AutoResumeReport {
            plan: plan.clone(),
            ..AutoResumeReport::default()
        };
        if !self.runtime_options.auto_resume_on_usage_limit {
            return Ok(report);
        }
        for run_id in &plan.stale {
            if self.get_run(run_id).is_none() {
                continue;
            }
            self.update_run(
                run_id,
                RunPatch::new()
                    .clear("autoResumeAt")
                    .clear("autoResumeAttempts"),
            )?;
            self.append_event(
                run_id,
                EventInput::new("note")
                    .field("message", "automatic resume retired during reconciliation"),
            )?;
            report.retired.push(run_id.clone());
        }
        for run_id in &plan.due {
            let attempts = self
                .get_run(run_id)
                .and_then(|run| run.auto_resume_attempts)
                .unwrap_or(0.0)
                + 1.0;
            let result = self.continue_run(
                run_id,
                ContinueOptions {
                    text: Some(
                        "The provider usage limit has reset. Continue the task from its last durable state."
                            .to_owned(),
                    ),
                    ..ContinueOptions::default()
                },
            )?;
            if result.ok {
                let status = self.get_run(run_id).map(|run| run.status);
                if matches!(
                    status,
                    Some(RunStatus::Queued | RunStatus::Running | RunStatus::Waiting)
                ) {
                    self.update_run(run_id, RunPatch::new().set("autoResumeAttempts", attempts))?;
                    self.append_event(
                        run_id,
                        EventInput::new("lifecycle").field(
                            "message",
                            format!("usage limit reset — resuming automatically ({attempts:.0}/{MAX_AUTO_RESUME_ATTEMPTS:.0})"),
                        ),
                    )?;
                    report.requeued.push(run_id.clone());
                } else if matches!(status, Some(RunStatus::Done | RunStatus::Review)) {
                    report.requeued.push(run_id.clone());
                } else {
                    report.retired.push(run_id.clone());
                }
            } else {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .clear("autoResumeAt")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("note").field(
                        "message",
                        format!(
                            "automatic resume could not start — {}",
                            result.error.unwrap_or_else(|| "unknown".to_owned())
                        ),
                    ),
                )?;
                report.retired.push(run_id.clone());
            }
        }
        self.pump()?;
        Ok(report)
    }

    /// Re-adopt durable live records after a process restart. No session object is assumed to
    /// survive: queued work is rebuilt from `workflow_def`, waiting work is settled, and running
    /// work is marked interrupted before it is requeued through the normal continuation path.
    pub fn recover(&mut self) -> io::Result<RecoveryReport> {
        let mut live = self
            .list_runs()
            .into_iter()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued | RunStatus::Waiting | RunStatus::Running
                )
            })
            .collect::<Vec<_>>();
        live.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut report = RecoveryReport::default();
        for run in live {
            if self.is_active(&run.id) {
                continue;
            }
            match run.status {
                RunStatus::Queued => {
                    let Some(workflow) = run.workflow_def.clone() else {
                        self.fail_run(
                            &run.id,
                            None,
                            "interrupted — workflow definition not recoverable after a restart"
                                .to_owned(),
                        )?;
                        report.failed.push(run.id);
                        continue;
                    };
                    let start_index = workflow
                        .steps
                        .iter()
                        .position(|step| {
                            self.get_run(&run.id)
                                .and_then(|record| {
                                    record
                                        .steps
                                        .iter()
                                        .find(|candidate| candidate.id == step.id)
                                })
                                .is_none_or(|record| record.status != StepStatus::Done)
                        })
                        .unwrap_or(workflow.steps.len());
                    self.jobs.insert(
                        run.id.clone(),
                        RuntimeJob::Workflow {
                            workflow,
                            start_index,
                            retry_counts: BTreeMap::new(),
                        },
                    );
                    self.enqueue(run.id.clone());
                    report.queued.push(run.id);
                }
                RunStatus::Waiting => {
                    let finished_at = now_iso8601();
                    for step in &run.steps {
                        if matches!(step.status, StepStatus::Waiting | StepStatus::Running) {
                            self.update_step(
                                &run.id,
                                &step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Done)
                                    .set("finishedAt", finished_at.clone()),
                            )?;
                        }
                    }
                    self.append_event(
                        &run.id,
                        EventInput::new("lifecycle")
                            .field("message", "process restarted — waiting session settled"),
                    )?;
                    self.settle_success(&run.id)?;
                    report.settled.push(run.id);
                }
                RunStatus::Running => {
                    let session_exists = run.steps.iter().any(|step| step.session_id.is_some());
                    let finished_at = now_iso8601();
                    for step in &run.steps {
                        if matches!(step.status, StepStatus::Running | StepStatus::Waiting) {
                            self.update_step(
                                &run.id,
                                &step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Failed)
                                    .set("finishedAt", finished_at.clone()),
                            )?;
                        }
                    }
                    self.update_run(
                        &run.id,
                        RunPatch::new()
                            .set("status", RunStatus::Failed)
                            .set("error", "interrupted — process exited during the run")
                            .set("finishedAt", finished_at)
                            .clear("currentStepId"),
                    )?;
                    if session_exists {
                        let result = self.continue_run(
                            &run.id,
                            ContinueOptions {
                                text: Some(
                                    "The process restarted while this task was running. Continue from the last durable state."
                                        .to_owned(),
                                ),
                                ..ContinueOptions::default()
                            },
                        )?;
                        self.append_event(
                            &run.id,
                            EventInput::new("lifecycle").field(
                                "message",
                                if result.ok {
                                    "process restarted — interrupted task requeued"
                                } else {
                                    "process restarted — interrupted task could not resume"
                                },
                            ),
                        )?;
                        if result.ok {
                            report.resumed.push(run.id);
                        } else {
                            report.failed.push(run.id);
                        }
                    } else {
                        self.append_event(
                            &run.id,
                            EventInput::new("lifecycle").field(
                                "message",
                                "process restarted — no session was available for continuation",
                            ),
                        )?;
                        report.failed.push(run.id);
                    }
                }
                RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review => {
                    continue;
                }
            }
        }
        let _ = self.reconcile_auto_resumes(&now_iso8601())?;
        self.run_to_completion()?;
        Ok(report)
    }

    /// Drain queued jobs while an injected runtime slot is available. The method is synchronous on
    /// purpose: the engine can call it from its scheduler, while unit tests can observe every
    /// transition without sleeps or a process-wide executor.
    /// Admit as much of the queue as current capacity allows. This never opens a session or runs
    /// a provider turn — `execute_job` stops the moment a job needs a live session and records an
    /// [`AdmittedTurn`] instead, so this always returns quickly regardless of how long a provider
    /// turn takes. A caller that wants those turns to actually run must drain
    /// [`Self::take_pending_turns`] afterward and dispatch each one to its own worker, outside any
    /// lock on this manager.
    pub fn pump(&mut self) -> io::Result<()> {
        loop {
            if !self.capacity_available() {
                break;
            }
            let Some(run_id) = self.queue.take_next() else {
                break;
            };
            let Some(job) = self.jobs.remove(&run_id) else {
                self.queue.finish_start(&run_id);
                continue;
            };
            if !self.try_acquire_resources(&run_id) {
                self.jobs.insert(run_id.clone(), job);
                self.queue.finish_start(&run_id);
                self.queue.push_front(run_id);
                break;
            }
            let result = self.execute_job(&run_id, job);
            self.queue.finish_start(&run_id);
            result?;
        }
        Ok(())
    }

    /// Drain every turn `pump` admitted but did not run. A caller must call this after any
    /// operation that could have called `pump` (directly or via a durable-state transition that
    /// re-admits queued work) and dispatch each returned turn to its own worker.
    pub fn take_pending_turns(&mut self) -> Vec<AdmittedTurn> {
        self.pending_turns.drain(..).collect()
    }

    /// Single-threaded stand-in for the production per-run worker coordinator: opens and runs
    /// every admitted turn synchronously (against whatever `session_factory` was configured),
    /// applying results the same way a concurrent worker would, until nothing is admittable. This
    /// is what a genuinely single-shot, no-TUI caller (the headless `coducktor run` CLI, and this
    /// module's own tests) wants — one turn at a time, blocking the caller until the whole
    /// workflow settles, with no separate coordinator thread to stand up.
    pub fn run_to_completion(&mut self) -> io::Result<()> {
        loop {
            self.pump()?;
            let admitted = self.take_pending_turns();
            if admitted.is_empty() {
                return Ok(());
            }
            for turn in admitted {
                self.drive_admitted_turn_sync(turn)?;
            }
        }
    }

    fn drive_admitted_turn_sync(&mut self, admitted: AdmittedTurn) -> io::Result<()> {
        let run_id = admitted.run_id.clone();
        let mut step_id = admitted.step_id.clone();
        let mut factory = self.session_factory.take();
        let opened = match factory.as_mut() {
            Some(factory) => factory.open(admitted.request.clone()),
            None => Err("session factory unavailable".to_owned()),
        };
        self.session_factory = factory;
        let cancellation_requested = admitted.request.cancellation.is_requested();
        let mut session = match opened {
            Ok(session) => session,
            Err(error) => {
                return self.apply_open_failure(admitted, error, cancellation_requested);
            }
        };
        let turn_result =
            session.turn(&mut |event| self.apply_turn_event(&run_id, &step_id, event));
        let mut step =
            self.apply_admitted_turn(admitted, session, turn_result, cancellation_requested)?;
        loop {
            let TurnStep::Nudge(boxed) = step else {
                if matches!(step, TurnStep::GitAutoCommit(_)) {
                    self.finish_git_auto(
                        &run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                }
                break;
            };
            let mut active = *boxed;
            step_id = active.workflow.steps[active.step_index].id.clone();
            let send_result =
                active
                    .session_mut()
                    .send_message(AUTONOMOUS_NUDGE, &[], &mut |event| {
                        self.apply_turn_event(&run_id, &step_id, event)
                    });
            step = self.apply_active_turn(&run_id, active, send_result, false)?;
        }
        Ok(())
    }

    fn runtime_busy_slots(&self) -> usize {
        self.active
            .values()
            .filter(|active| active.holds_slot)
            .count()
            .saturating_add(self.queue.starting().count())
            .saturating_add(self.in_flight.len())
    }

    fn capacity_available(&self) -> bool {
        if self.runtime_busy_slots() >= self.runtime_options.max_parallel {
            return false;
        }
        self.workspace_semaphore
            .as_ref()
            .is_none_or(|semaphore| semaphore.busy_slots() < semaphore.max_parallel())
    }

    fn try_acquire_resources(&mut self, run_id: &str) -> bool {
        let project_id = self.project_id.clone();
        if !self.workspace_holds.contains(run_id) {
            let acquired = self
                .workspace_semaphore
                .as_mut()
                .is_none_or(|semaphore| semaphore.try_acquire(run_id, &project_id));
            if !acquired {
                return false;
            }
            if self.workspace_semaphore.is_some() {
                self.workspace_holds.insert(run_id.to_owned());
            }
        }
        // A worktree has an independent checkout, so it must not be serialized with another
        // worktree (or an in-place run) merely because both came from the same repository. The
        // integration layer still installs the root lease for in-place runs, whose checkout is
        // shared and therefore unsafe to mutate concurrently.
        let needs_repository_lease = self
            .get_run(run_id)
            .is_none_or(|run| run.worktree_path.is_none());
        if needs_repository_lease && !self.repository_holds.contains(run_id) {
            let acquired = self
                .repository_lease
                .as_mut()
                .is_none_or(|lease| lease.try_acquire(run_id));
            if !acquired {
                self.release_workspace_hold(run_id);
                return false;
            }
            if self.repository_lease.is_some() {
                self.repository_holds.insert(run_id.to_owned());
            }
        }
        true
    }

    fn release_workspace_hold(&mut self, run_id: &str) {
        if self.workspace_holds.remove(run_id)
            && let Some(semaphore) = self.workspace_semaphore.as_mut()
        {
            semaphore.release(run_id, &self.project_id);
        }
    }

    fn try_acquire_workspace_resume(&mut self, run_id: &str) -> bool {
        if self.workspace_holds.contains(run_id) {
            return true;
        }
        let acquired = self
            .workspace_semaphore
            .as_mut()
            .is_none_or(|semaphore| semaphore.try_acquire(run_id, &self.project_id));
        if acquired && self.workspace_semaphore.is_some() {
            self.workspace_holds.insert(run_id.to_owned());
        }
        acquired
    }

    fn release_repository_hold(&mut self, run_id: &str) {
        if self.repository_holds.remove(run_id)
            && let Some(lease) = self.repository_lease.as_mut()
        {
            lease.release(run_id);
        }
    }

    fn execute_job(&mut self, run_id: &str, job: RuntimeJob) -> io::Result<()> {
        let (workflow, mut index, mut retry_counts, continuation) = match job {
            RuntimeJob::Workflow {
                workflow,
                start_index,
                retry_counts,
            } => (workflow, start_index, retry_counts, None),
            RuntimeJob::Continuation {
                workflow,
                step_index,
                session_id,
                prompt,
                images,
                runner,
                model,
                retry_counts,
            } => (
                workflow,
                step_index,
                retry_counts,
                Some((session_id, prompt, images, runner, model)),
            ),
        };
        let Some(record) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        let plan_checkpoint = self.plan_checkpoints.remove(run_id).unwrap_or_default();
        let mut pending_context_prompt = self.pending_context_prompts.remove(run_id);

        if continuation.is_some() {
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Running)
                    .clear("error")
                    .clear("finishedAt")
                    .clear("currentStepId")
                    .set("activity", Value::Null),
            )?;
        } else {
            let started_at = record.started_at.unwrap_or_else(now_iso8601);
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Running)
                    .set("startedAt", started_at)
                    .clear("error")
                    .clear("finishedAt"),
            )?;
            while index < workflow.steps.len()
                && self
                    .get_run(run_id)
                    .and_then(|run| run.steps.get(index))
                    .is_some_and(|step| step.status == StepStatus::Done)
            {
                index += 1;
            }
        }

        let mut continuation_prompt = continuation
            .as_ref()
            .map(|(_, prompt, _, _, _)| prompt.clone());
        let mut continuation_images = continuation
            .as_ref()
            .map(|(_, _, images, _, _)| images.clone())
            .unwrap_or_default();
        let continuation_session = continuation
            .as_ref()
            .and_then(|(session_id, _, _, _, _)| session_id.clone());
        let continuation_runner = continuation.as_ref().map(|(_, _, _, runner, _)| *runner);
        let continuation_model = continuation
            .as_ref()
            .and_then(|(_, _, _, _, model)| model.clone());
        let continuation_step = continuation.is_some();

        while index < workflow.steps.len() {
            let step = workflow.steps[index].clone();
            self.update_run(
                run_id,
                RunPatch::new().set("currentStepId", step.id.clone()),
            )?;
            let iteration = self
                .get_run(run_id)
                .and_then(|run| run.steps.iter().find(|candidate| candidate.id == step.id))
                .map(|step| step.iterations + 1.0)
                .unwrap_or(1.0);
            self.update_step(
                run_id,
                &step.id,
                StepPatch::new()
                    .set("status", StepStatus::Running)
                    .set("iterations", iteration)
                    .set("startedAt", now_iso8601())
                    .clear("finishedAt")
                    .clear("error"),
            )?;
            self.append_step_event(run_id, &step, "step-start", iteration)?;

            if let Some(command) = step.command.as_deref() {
                let cwd = self
                    .get_run(run_id)
                    .and_then(|run| run.worktree_path.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.repo_root());
                let result = match self.check_executor.as_mut() {
                    Some(executor) => executor.run(command, &cwd),
                    None => Err("check executor unavailable".to_owned()),
                };
                let result = match result {
                    Ok(result) => result,
                    Err(error) => CheckResult {
                        success: false,
                        exit_code: 1,
                        output: error,
                    },
                };
                self.append_event(
                    run_id,
                    EventInput::new("check-output")
                        .step(step.id.clone())
                        .field("command", command)
                        .field("text", result.output.clone())
                        .field("exitCode", result.exit_code),
                )?;
                if result.success {
                    self.complete_step(run_id, &step.id, None)?;
                    index += 1;
                    continue;
                }

                let used = retry_counts.get(&step.id).copied().unwrap_or(0);
                let retry_target = step
                    .on_fail
                    .as_ref()
                    .filter(|policy| lifecycle::retry_allowed(used, policy.max))
                    .and_then(|policy| {
                        workflow
                            .steps
                            .iter()
                            .position(|candidate| candidate.id == policy.retry)
                            .map(|target| (target, policy.retry.clone(), used + 1, policy.max))
                    });
                if let Some((target, target_id, attempt, max)) = retry_target {
                    retry_counts.insert(step.id.clone(), attempt);
                    self.update_step(
                        run_id,
                        &step.id,
                        StepPatch::new()
                            .set("status", StepStatus::Failed)
                            .set("error", "check failed — looping back")
                            .set("finishedAt", now_iso8601()),
                    )?;
                    for retry_index in target..=index {
                        if let Some(retry_step) = workflow.steps.get(retry_index) {
                            self.update_step(
                                run_id,
                                &retry_step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Pending)
                                    .clear("error")
                                    .clear("finishedAt"),
                            )?;
                        }
                    }
                    self.append_event(
                        run_id,
                        EventInput::new("note")
                            .step(step.id.clone())
                            .field(
                                "message",
                                format!(
                                    "check failed — retrying from \"{target_id}\" (attempt {attempt}/{max})"
                                ),
                            ),
                    )?;
                    index = target;
                    continue;
                }

                let attempts = used + 1;
                self.fail_run(
                    run_id,
                    Some(&step.id),
                    format!(
                        "check \"{}\" failed{}",
                        step.id,
                        step.on_fail
                            .as_ref()
                            .map(|_| format!(" after {attempts} attempts"))
                            .unwrap_or_default()
                    ),
                )?;
                return Ok(());
            }

            let task = self
                .get_run(run_id)
                .map(hydrate_queued_prompt)
                .unwrap_or_default();
            let prompt = pending_context_prompt
                .take()
                .or_else(|| continuation_prompt.take())
                .unwrap_or_else(|| {
                    apply_template(step.prompt.as_deref().unwrap_or("{{task}}"), &task)
                });
            let prompt = if !continuation_step {
                if let Some(note) = types::chain_step_note(&workflow.steps, index) {
                    format!("{note}\n\n---\n\n{prompt}")
                } else {
                    prompt
                }
            } else {
                prompt
            };
            let requested_runner = continuation_runner
                .or(step.runner)
                .or_else(|| self.get_run(run_id).and_then(|run| run.requested_runner))
                .unwrap_or(RunnerSelection::Claude);
            let runner = if requested_runner == RunnerSelection::Auto {
                self.get_run(run_id)
                    .and_then(|run| run.runner)
                    .map(runner_selection)
                    .unwrap_or(RunnerSelection::Claude)
            } else {
                requested_runner
            };
            let model = continuation_model
                .clone()
                .or_else(|| self.get_run(run_id).and_then(|run| run.model.clone()));
            let session_id = if continuation_step
                && index
                    == workflow
                        .steps
                        .iter()
                        .position(|candidate| candidate.id == step.id)
                        .unwrap_or(index)
            {
                continuation_session.clone()
            } else {
                None
            };
            let allowed_tools = step.allowed_tools.clone().unwrap_or_else(|| {
                types::DEFAULT_ALLOWED_TOOLS
                    .iter()
                    .map(|tool| (*tool).to_owned())
                    .collect()
            });
            let bash_allowlist = step.bash_allowlist.clone().unwrap_or_default();
            let run_record = self.get_run(run_id);
            let system_prompt = Some(system_prompt_with_task_controls(
                run_record.and_then(|run| run.system_prompt.as_deref()),
            ));
            let reasoning_effort = run_record
                .and_then(|run| run.reasoning_effort)
                .and_then(concrete_reasoning_effort);
            let cancellation = CancellationToken::default();
            let retry_prompt = prompt.clone();
            let request = SessionRequest {
                run_id: run_id.to_owned(),
                step_id: step.id.clone(),
                prompt,
                images: if continuation_step {
                    std::mem::take(&mut continuation_images)
                } else {
                    self.get_run(run_id)
                        .map(hydrate_queued_images)
                        .unwrap_or_default()
                },
                runner,
                model,
                session_id,
                continuation: continuation_step,
                agent_profile: self
                    .get_run(run_id)
                    .and_then(|run| run.agent_profile.clone()),
                env: BTreeMap::new(),
                cwd: self
                    .get_run(run_id)
                    .and_then(|run| run.worktree_path.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.repo_root()),
                allowed_tools,
                bash_allowlist,
                system_prompt,
                reasoning_effort,
                cancellation: cancellation.clone(),
            };
            let mut run_affinity = RunPatch::new();
            if let Some(backend) = concrete_runner(runner) {
                run_affinity = run_affinity.set("runner", backend);
            }
            if self
                .get_run(run_id)
                .and_then(|run| run.requested_runner)
                .is_none()
            {
                run_affinity = run_affinity.set("requestedRunner", runner);
            }
            if !run_affinity.fields().is_empty() {
                self.update_run(run_id, run_affinity)?;
            }
            let mut step_affinity = StepPatch::new().set("requestedRunner", requested_runner);
            if let Some(backend) = concrete_runner(runner) {
                step_affinity = step_affinity.set("backend", backend);
            }
            if let Some(profile_id) = self
                .get_run(run_id)
                .and_then(|run| run.agent_profile.clone())
            {
                step_affinity = step_affinity.set("profileId", profile_id);
            }
            self.update_step(run_id, &step.id, step_affinity)?;
            let concrete = concrete_runner(runner).unwrap_or(Runner::Claude);
            if requested_runner == RunnerSelection::Auto {
                self.announce_auto_route(run_id, concrete, request.model.as_deref())?;
            }
            // Opening and running this turn is deliberately not done here: both can block for the
            // lifetime of a provider turn, and this function runs under the manager's lock. The
            // caller (a per-run worker, outside any lock) opens `request` and runs the turn, then
            // reports back through `apply_open_failure`/`apply_admitted_turn` so this workflow can
            // resume exactly where it left off.
            self.in_flight.insert(run_id.to_owned());
            self.pending_turns.push_back(AdmittedTurn {
                run_id: run_id.to_owned(),
                step_id: step.id.clone(),
                request,
                resume: PendingResume {
                    workflow,
                    index,
                    retry_counts,
                    plan_checkpoint,
                    concrete,
                    retry_prompt,
                },
            });
            return Ok(());
        }

        self.settle_success(run_id)
    }

    /// Requeue a run for the given step so a fresh `execute_job` call rebuilds and reattempts its
    /// turn — used by both open-failure and turn-failure auto-failover retries, which durably
    /// mutate the run's runner before asking for another attempt.
    fn requeue_for_retry(
        &mut self,
        run_id: &str,
        workflow: WorkflowDef,
        index: usize,
        retry_counts: BTreeMap<String, u32>,
        retry_prompt: String,
    ) -> io::Result<()> {
        self.pending_context_prompts
            .insert(run_id.to_owned(), retry_prompt);
        self.jobs.insert(
            run_id.to_owned(),
            RuntimeJob::Workflow {
                workflow,
                start_index: index,
                retry_counts,
            },
        );
        self.enqueue(run_id.to_owned());
        self.pump()
    }

    /// Apply the result of attempting to open a session for an [`AdmittedTurn`]. Mirrors the
    /// pre-refactor open-failure branch: cancellation wins outright, then auto-failover retries by
    /// requeuing (a fresh `execute_job` call rebuilds the request against the newly selected
    /// runner), otherwise the run fails.
    pub fn apply_open_failure(
        &mut self,
        admitted: AdmittedTurn,
        error: String,
        cancellation_requested: bool,
    ) -> io::Result<()> {
        let AdmittedTurn {
            run_id,
            step_id,
            resume,
            ..
        } = admitted;
        self.in_flight.remove(&run_id);
        if cancellation_requested {
            return self.cancel_run_after_session(&run_id, &step_id);
        }
        if self.try_auto_failover(&run_id, &step_id, resume.concrete, &error, true)? {
            return self.requeue_for_retry(
                &run_id,
                resume.workflow,
                resume.index,
                resume.retry_counts,
                resume.retry_prompt,
            );
        }
        self.fail_run(&run_id, Some(&step_id), error)
    }

    /// Fail a run that a caller admitted but could not even hand to a worker — e.g. the OS
    /// refused to start a thread. A local resource failure, not a provider one, so unlike
    /// [`Self::apply_open_failure`] this never retries through auto-failover; it just needs the
    /// run's id, not the full [`AdmittedTurn`] (deliberately, so a caller that already lost the
    /// value moving it toward a worker that never started can still report the failure).
    pub fn fail_admission(
        &mut self,
        run_id: &str,
        step_id: &str,
        reason: String,
    ) -> io::Result<()> {
        self.in_flight.remove(run_id);
        self.fail_run(run_id, Some(step_id), reason)
    }

    /// Apply the result of a successfully opened session's first turn. `turn_result` is exactly
    /// what `AgentSession::turn` returned, run entirely outside the manager's lock by the caller.
    pub fn apply_admitted_turn(
        &mut self,
        admitted: AdmittedTurn,
        session: Box<dyn AgentSession + Send>,
        turn_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let AdmittedTurn {
            run_id,
            step_id: _,
            resume,
            ..
        } = admitted;
        let outcome = match turn_result {
            Ok(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Ok(outcome) => outcome,
            Err(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Err(error) => {
                self.in_flight.remove(&run_id);
                let step_id = resume.workflow.steps[resume.index].id.clone();
                if self.try_auto_failover(&run_id, &step_id, resume.concrete, &error, false)? {
                    self.requeue_for_retry(
                        &run_id,
                        resume.workflow,
                        resume.index,
                        resume.retry_counts,
                        resume.retry_prompt,
                    )?;
                    return Ok(TurnStep::Done);
                }
                self.fail_run(&run_id, Some(&step_id), error)?;
                return Ok(TurnStep::Done);
            }
        };
        let active = RuntimeActive {
            workflow: resume.workflow,
            step_index: resume.index,
            next_index: resume.index + 1,
            retry_counts: resume.retry_counts,
            session,
            holds_slot: true,
            plan_checkpoint: resume.plan_checkpoint,
            auto_continues: 0,
            failover: Some(FailoverContext {
                concrete: resume.concrete,
                retry_prompt: resume.retry_prompt,
            }),
        };
        self.continue_active_turn(&run_id, active, outcome)
    }

    /// Apply the result of an autonomous nudge (`AgentSession::send_message`) the caller sent
    /// after a prior [`TurnStep::Nudge`]. Behaves exactly like the initial turn's wrapping: a
    /// cancellation request wins over whatever the session actually returned.
    pub fn apply_active_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        send_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let outcome = match send_result {
            Ok(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Ok(outcome) => outcome,
            Err(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Err(error) => SessionOutcome::Failed {
                message: error,
                report: SessionReport::default(),
            },
        };
        self.continue_active_turn(run_id, active, outcome)
    }

    /// Every currently live-in-process monitoring session whose durable `monitoringWakeAt`
    /// deadline has passed. Read-only and cheap — a caller (a dedicated scheduler, not this
    /// manager's own admission loop) polls this on a bounded interval and dispatches each one
    /// through [`Self::begin_monitoring_wake`] the same way it would an [`AdmittedTurn`].
    pub fn due_monitoring_wakes(&self, now: &str) -> Vec<String> {
        self.runs
            .values()
            .filter(|run| self.active.contains_key(&run.id))
            .filter(|run| monitoring::is_due(run, now))
            .map(|run| run.id.clone())
            .collect()
    }

    /// Detach a due monitoring session from `self.active` so its check-in turn can run outside
    /// this manager's lock, exactly like a freshly admitted turn. Re-validates against the
    /// current durable record rather than trusting an earlier `due_monitoring_wakes` call, since
    /// state can change between planning and dispatch (a real user message, a cancel). Counts as
    /// `in_flight` for the same reason an admitted turn does: `RunManager::cancel` must not race
    /// the caller's eventual `apply_active_turn` report by settling the run out from under it.
    pub fn begin_monitoring_wake(&mut self, run_id: &str) -> io::Result<Option<RuntimeActive>> {
        if self.get_run(run_id).and_then(|run| run.activity) != Some(RunActivity::Monitoring) {
            return Ok(None);
        }
        if !self.active.contains_key(run_id) {
            return Ok(None);
        }
        self.append_event(
            run_id,
            EventInput::new("note").field("message", "monitoring check-in"),
        )?;
        self.in_flight.insert(run_id.to_owned());
        Ok(self.active.remove(run_id))
    }

    /// A monitoring wake's worker could not even be started (e.g. the OS refused a new thread).
    /// Put the still-live session back exactly as `park_session` would have left it, rather than
    /// leaking it as permanently `in_flight` with no session anywhere to resolve it — the next
    /// scheduler pass, or an explicit cancel/message, can still reach it normally.
    pub fn abandon_monitoring_wake(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        self.active.insert(run_id.to_owned(), active);
    }

    fn apply_session_report(
        &mut self,
        run_id: &str,
        step_id: &str,
        report: &SessionReport,
        fallback_session_id: Option<String>,
    ) -> io::Result<()> {
        let Some(step) = self
            .get_run(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == step_id))
            .cloned()
        else {
            return Ok(());
        };
        let mut patch = StepPatch::new();
        if let Some(session_id) = report.session_id.clone().or(fallback_session_id) {
            patch = patch.set("sessionId", session_id);
        }
        if report.tokens_used.is_finite() && report.tokens_used != 0.0 {
            patch = patch.set("tokensUsed", step.tokens_used + report.tokens_used);
        }
        if let Some(input) = report
            .input_tokens
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            patch = patch.set("inputTokens", step.input_tokens.unwrap_or(0.0) + input);
        }
        if let Some(output) = report
            .output_tokens
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            patch = patch.set("outputTokens", step.output_tokens.unwrap_or(0.0) + output);
        }
        if let Some(cost) = report.cost_usd.filter(|value| value.is_finite()) {
            patch = patch.set("costUsd", step.cost_usd.unwrap_or(0.0) + cost);
        }
        if !patch.fields().is_empty() {
            self.update_step(run_id, step_id, patch)?;
        }
        Ok(())
    }

    /// Post-turn marker bookkeeping (`DUCK:DONE`/`DUCK:PR=`/…) over the whole aggregated turn text.
    /// This no longer persists the text itself as an event — the live [`Self::event_sink`]
    /// already streamed it turn-by-turn as the session produced it; re-appending the aggregate
    /// here would duplicate the transcript.
    fn apply_session_markers(&mut self, run_id: &str, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.apply_turn_markers(run_id, text).map(|_| ())
    }

    fn append_step_event(
        &mut self,
        run_id: &str,
        step: &coducktor_contract::workflows::WorkflowStepDef,
        event_type: &str,
        iteration: f64,
    ) -> io::Result<()> {
        self.append_event(
            run_id,
            EventInput::new(event_type)
                .step(step.id.clone())
                .field("name", step.name.clone().unwrap_or_else(|| step.id.clone()))
                .field(
                    "kind",
                    if step.command.is_some() {
                        StepKind::Check
                    } else {
                        StepKind::Agent
                    },
                )
                .field("iteration", iteration),
        )?;
        Ok(())
    }

    fn complete_step(
        &mut self,
        run_id: &str,
        step_id: &str,
        error: Option<&str>,
    ) -> io::Result<()> {
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Done)
                .set("finishedAt", now_iso8601())
                .set("error", error),
        )?;
        self.append_event(
            run_id,
            EventInput::new("step-end")
                .step(step_id.to_owned())
                .field("status", StepStatus::Done),
        )?;
        Ok(())
    }

    fn park_session(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        requested_monitoring: bool,
    ) -> io::Result<()> {
        // Failover eligibility belongs to the live turn that hit the failure, never to a later,
        // separately triggered resume (`deliver_message`/`finish`) of this same parked session.
        active.failover = None;
        let monitoring = requested_monitoring
            && self
                .runs
                .iter()
                .filter(|(id, run)| {
                    id.as_str() != run_id && run.activity == Some(RunActivity::Monitoring)
                })
                .count()
                < self.runtime_options.max_monitoring_sessions;
        if active.holds_slot {
            let _ = self.try_acquire_workspace_resume(run_id);
        } else {
            self.release_workspace_hold(run_id);
        }
        let running = monitoring || active.holds_slot;
        let status = if running {
            RunStatus::Running
        } else {
            RunStatus::Waiting
        };
        let activity = monitoring.then_some(RunActivity::Monitoring);
        let monitoring_wake_at = monitoring.then(|| {
            self.runtime_options
                .monitoring_wake_interval_minutes
                .map(|minutes| now_plus_iso8601(Duration::from_secs(minutes.saturating_mul(60))))
        });
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", status)
                .set("activity", activity)
                .set("monitoringWakeAt", monitoring_wake_at.flatten()),
        )?;
        self.update_step(
            run_id,
            &active.workflow.steps[active.step_index].id,
            StepPatch::new().set(
                "status",
                if running {
                    StepStatus::Running
                } else {
                    StepStatus::Waiting
                },
            ),
        )?;
        self.active.insert(run_id.to_owned(), active);
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field(
                "message",
                if monitoring {
                    "session parked for monitoring"
                } else if running {
                    "session remains active"
                } else {
                    "session waiting for input"
                },
            ),
        )?;
        Ok(())
    }

    /// Apply one turn's outcome to a live, in-progress session and decide what happens next.
    /// Shared by both a freshly admitted turn's worker (`apply_admitted_turn`/`apply_active_turn`,
    /// where `active.failover` carries the original open attempt's failover eligibility) and the
    /// synchronous `deliver_message`/`finish` resume of an already-parked session (where it is
    /// always `None` — auto-failover never applies to a resumed session, matching prior
    /// behavior). A `Nudge` result means the caller must run one more send_message turn itself
    /// (outside any lock for a worker; inline, still under the lock, for a synchronous resume)
    /// and call this again with the result.
    fn continue_active_turn(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        outcome: SessionOutcome,
    ) -> io::Result<TurnStep> {
        let step_id = active.workflow.steps[active.step_index].id.clone();
        let report = session_outcome_report(&outcome).clone();
        self.apply_session_report(run_id, &step_id, &report, active.session.session_id())?;
        self.apply_session_markers(run_id, &report.turn_text)?;
        let refresh_prompt = if self.intelligent_context_refresh {
            report.plan_entries.as_deref().and_then(|entries| {
                context_refresh::observe_plan(&mut active.plan_checkpoint, entries, true)
            })
        } else {
            None
        };
        if let Some(refresh_prompt) = refresh_prompt
            && matches!(
                &outcome,
                SessionOutcome::Completed(_) | SessionOutcome::Waiting(_)
            )
        {
            self.update_step(
                run_id,
                &step_id,
                StepPatch::new()
                    .set("status", StepStatus::Pending)
                    .clear("finishedAt")
                    .clear("error"),
            )?;
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Queued)
                    .clear("currentStepId")
                    .clear("finishedAt"),
            )?;
            self.release_workspace_hold(run_id);
            self.plan_checkpoints
                .insert(run_id.to_owned(), active.plan_checkpoint);
            self.pending_context_prompts
                .insert(run_id.to_owned(), refresh_prompt);
            self.jobs.insert(
                run_id.to_owned(),
                RuntimeJob::Workflow {
                    workflow: active.workflow,
                    start_index: active.step_index,
                    retry_counts: active.retry_counts,
                },
            );
            self.enqueue(run_id.to_owned());
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    "intelligent context refresh — reopening a fresh session",
                ),
            )?;
            self.in_flight.remove(run_id);
            self.pump()?;
            return Ok(TurnStep::Done);
        }
        let should_nudge = self
            .get_run(run_id)
            .is_some_and(|run| run.autonomous == Some(true))
            && matches!(
                &outcome,
                SessionOutcome::Waiting(report)
                    if report.decision.is_none()
                        || report.decision == Some(TurnMarkerDecision::Waiting)
            )
            && active.auto_continues < MAX_AUTONOMOUS_CONTINUES;
        if should_nudge {
            active.auto_continues += 1;
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "autonomous pass {} of {}",
                        active.auto_continues, MAX_AUTONOMOUS_CONTINUES
                    ),
                ),
            )?;
            return Ok(TurnStep::Nudge(Box::new(active)));
        }
        match outcome {
            SessionOutcome::Failed { message, .. } => {
                self.in_flight.remove(run_id);
                if let Some(failover) = active.failover.clone()
                    && self.try_auto_failover(
                        run_id,
                        &step_id,
                        failover.concrete,
                        &message,
                        false,
                    )?
                {
                    self.requeue_for_retry(
                        run_id,
                        active.workflow,
                        active.step_index,
                        active.retry_counts,
                        failover.retry_prompt,
                    )?;
                    return Ok(TurnStep::Done);
                }
                self.fail_run(run_id, Some(&step_id), message)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Cancelled(_) => {
                self.in_flight.remove(run_id);
                self.cancel_run_after_session(run_id, &step_id)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Running(_) => {
                self.in_flight.remove(run_id);
                active.holds_slot = true;
                self.park_session(run_id, active, false)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Waiting(report) => {
                self.in_flight.remove(run_id);
                active.holds_slot = false;
                self.park_session(
                    run_id,
                    active,
                    report.decision == Some(TurnMarkerDecision::Monitoring),
                )?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Completed(_) => {
                self.in_flight.remove(run_id);
                self.complete_step(run_id, &step_id, None)?;
                if active.next_index < active.workflow.steps.len() {
                    self.update_run(
                        run_id,
                        RunPatch::new()
                            .set("status", RunStatus::Queued)
                            .clear("finishedAt")
                            .clear("currentStepId"),
                    )?;
                    self.jobs.insert(
                        run_id.to_owned(),
                        RuntimeJob::Workflow {
                            workflow: active.workflow,
                            start_index: active.next_index,
                            retry_counts: active.retry_counts,
                        },
                    );
                    self.enqueue(run_id.to_owned());
                } else {
                    if self.should_prepare_git_auto_commit(run_id) {
                        self.append_event(
                            run_id,
                            EventInput::new("note")
                                .field("message", "preparing automatic commit message"),
                        )?;
                        return Ok(TurnStep::GitAutoCommit(Box::new(active)));
                    }
                    self.settle_success(run_id)?;
                }
                self.pump()?;
                Ok(TurnStep::Done)
            }
        }
    }

    fn should_prepare_git_auto_commit(&mut self, run_id: &str) -> bool {
        let Some(run) = self.get_run(run_id).cloned() else {
            return false;
        };
        run.git_auto == Some(true)
            && self
                .diff_inspector
                .as_mut()
                .is_some_and(|inspector| inspector.has_diff(&run))
    }

    /// Record the synthetic commit-subject turn and return its subject to the production
    /// dispatcher. The caller must then either call [`Self::finish_git_auto`] after its Git work
    /// or use the returned error as that method's failure reason.
    pub fn apply_git_auto_commit_message(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        outcome: Result<SessionOutcome, String>,
    ) -> io::Result<Result<GitAutoMessage, String>> {
        self.in_flight.remove(run_id);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(message) => return Ok(Err(message)),
        };
        let report = session_outcome_report(&outcome).clone();
        let step_id = active.workflow.steps[active.step_index].id.clone();
        self.apply_session_report(run_id, &step_id, &report, active.session.session_id())?;
        self.apply_session_markers(run_id, &report.turn_text)?;
        match outcome {
            SessionOutcome::Cancelled(_) => {
                self.cancel_run_after_session(run_id, &step_id)?;
                Ok(Ok(GitAutoMessage::Cancelled))
            }
            SessionOutcome::Failed { message, .. } => Ok(Err(message)),
            SessionOutcome::Completed(_)
            | SessionOutcome::Running(_)
            | SessionOutcome::Waiting(_) => {
                Ok(commit_subject(&report.turn_text).map(GitAutoMessage::Subject))
            }
        }
    }

    /// Settle the Git action that follows a successful automatic commit-subject turn. A failure
    /// deliberately leaves the completed changes in Review so the user can inspect, commit, or
    /// push them manually instead of losing a successful agent run to a Git configuration issue.
    pub fn finish_git_auto(&mut self, run_id: &str, result: Result<(), String>) -> io::Result<()> {
        match result {
            Ok(()) => {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Done)
                        .set("finishedAt", now_iso8601())
                        .clear("currentStepId")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle")
                        .field("message", "automatic commit and push finished"),
                )?;
                self.cleanup_runtime(run_id);
                Ok(())
            }
            Err(reason) => {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Review)
                        .set("finishedAt", now_iso8601())
                        .clear("currentStepId")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("note").field(
                        "message",
                        format!(
                            "automatic commit/push failed — review and finish manually: {reason}"
                        ),
                    ),
                )?;
                self.cleanup_runtime(run_id);
                Ok(())
            }
        }
    }

    /// The worktree is preferred; single-worktree runs use the manager's repository root.
    pub fn working_directory_for(&self, run: &RunRecord) -> Option<PathBuf> {
        run.worktree_path
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.repo_root.clone())
    }

    fn announce_auto_route(
        &mut self,
        run_id: &str,
        runner: Runner,
        model: Option<&str>,
    ) -> io::Result<()> {
        if self.auto_routes.contains_key(run_id) {
            let model = model.unwrap_or("provider default");
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "Auto routing · trying {} · model {model}",
                        runner_label(runner)
                    ),
                ),
            )?;
        }
        Ok(())
    }

    /// Retire a provider that rejected an Auto request before useful work could complete and
    /// select the next engine-ranked candidate. Explicit runner requests never enter this path.
    fn try_auto_failover(
        &mut self,
        run_id: &str,
        step_id: &str,
        failed_runner: Runner,
        error: &str,
        opening_failed: bool,
    ) -> io::Result<bool> {
        if self.get_run(run_id).and_then(|run| run.requested_runner) != Some(RunnerSelection::Auto)
            || (!opening_failed && !is_auto_route_failure(error))
        {
            return Ok(false);
        }
        let next = self.auto_routes.get_mut(run_id).and_then(|candidates| {
            candidates.retain(|candidate| *candidate != failed_runner);
            candidates.first().copied()
        });
        let Some(next) = next else {
            if self.auto_routes.contains_key(run_id) {
                self.append_event(
                    run_id,
                    EventInput::new("note")
                        .field("noteKind", "provider-switch")
                        .field("message", "Auto routing · no eligible providers remain"),
                )?;
            }
            return Ok(false);
        };
        self.update_run(run_id, RunPatch::new().set("runner", next).clear("model"))?;
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Pending)
                .clear("backend")
                .clear("sessionId")
                .clear("error")
                .clear("finishedAt"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("note")
                .step(step_id.to_owned())
                .field("noteKind", "provider-switch")
                .field(
                    "message",
                    format!(
                        "Auto routing · {} {} — trying {}",
                        runner_label(failed_runner),
                        auto_route_failure_reason(error),
                        runner_label(next)
                    ),
                ),
        )?;
        Ok(true)
    }

    fn fail_run(&mut self, run_id: &str, step_id: Option<&str>, message: String) -> io::Result<()> {
        let finished_at = now_iso8601();
        if let Some(step_id) = step_id {
            self.update_step(
                run_id,
                step_id,
                StepPatch::new()
                    .set("status", StepStatus::Failed)
                    .set("error", message.clone())
                    .set("finishedAt", finished_at.clone()),
            )?;
        }
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Failed)
                .set("error", message.clone())
                .set("finishedAt", finished_at)
                .clear("currentStepId"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", format!("run failed — {message}")),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    fn cancel_run_after_session(&mut self, run_id: &str, step_id: &str) -> io::Result<()> {
        let finished_at = now_iso8601();
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Cancelled)
                .set("finishedAt", finished_at.clone()),
        )?;
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Cancelled)
                .set("finishedAt", finished_at)
                .clear("currentStepId"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", "run cancelled"),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    fn cleanup_runtime(&mut self, run_id: &str) {
        self.queue.remove(run_id);
        self.jobs.remove(run_id);
        self.active.remove(run_id);
        self.usage.remove(run_id);
        self.plan_checkpoints.remove(run_id);
        self.pending_context_prompts.remove(run_id);
        self.auto_routes.remove(run_id);
        self.event_appenders.remove(run_id);
        self.release_workspace_hold(run_id);
        self.release_repository_hold(run_id);
    }

    fn settle_success(&mut self, run_id: &str) -> io::Result<()> {
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        let has_diff = self
            .diff_inspector
            .as_mut()
            .is_some_and(|inspector| inspector.has_diff(&run));
        let status = success_status(
            has_diff,
            self.runtime_options.review_gate,
            run.autonomous == Some(true),
        );
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", status)
                .set("finishedAt", now_iso8601())
                .clear("currentStepId")
                .clear("autoResumeAttempts"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field(
                "message",
                if status == RunStatus::Review {
                    "changes ready for review — send feedback, open a draft PR, or finish"
                } else {
                    "run finished"
                },
            ),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    /// Cancel queued work, an injected active session, or a live-status record
    /// with no process-local runtime (a run loaded from disk after a restart).
    /// Terminal cleanup removes all process-local queue/job/usage state while
    /// leaving the durable record as the source of truth.
    pub fn cancel(&mut self, run_id: &str) -> io::Result<bool> {
        // A worker already owns this run's session outside any lock this call holds; settling it
        // here would race the worker's own eventual `apply_open_failure`/`apply_admitted_turn`
        // report. Cancellation itself goes through the run's `CancellationToken`, not this call —
        // the caller signals that independently before ever reaching the manager lock. Once the
        // worker observes it, it reports back through the normal `Cancelled` outcome path.
        if self.in_flight.contains(run_id) {
            return Ok(true);
        }
        if self.queue.is_queued(run_id) || self.jobs.contains_key(run_id) {
            self.cleanup_runtime(run_id);
            if self.get_run(run_id).is_none() {
                return Ok(false);
            }
            let finished_at = now_iso8601();
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Cancelled)
                    .set("finishedAt", finished_at)
                    .clear("currentStepId"),
            )?;
            self.append_event(
                run_id,
                EventInput::new("lifecycle").field("message", "cancelled while queued"),
            )?;
            self.pump()?;
            return Ok(true);
        }
        let Some(mut active) = self.active.remove(run_id) else {
            if self.get_run(run_id).is_some_and(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued | RunStatus::Running | RunStatus::Waiting
                )
            }) {
                self.settle_steps(run_id, StepStatus::Cancelled)?;
                let finished_at = now_iso8601();
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Cancelled)
                        .set("finishedAt", finished_at)
                        .clear("currentStepId"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle").field("message", "run cancelled"),
                )?;
                self.cleanup_runtime(run_id);
                return Ok(true);
            }
            return Ok(false);
        };
        active.session.cancel();
        self.cancel_run_after_session(run_id, &active.workflow.steps[active.step_index].id)?;
        self.pump()?;
        Ok(true)
    }

    /// Settle every in-flight step of a run, so a settled record has no dangling
    /// live steps.
    fn settle_steps(&mut self, run_id: &str, status: StepStatus) -> io::Result<()> {
        let finished_at = now_iso8601();
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        for step in &run.steps {
            if matches!(
                step.status,
                StepStatus::Pending | StepStatus::Waiting | StepStatus::Running
            ) {
                self.update_step(
                    run_id,
                    &step.id,
                    StepPatch::new()
                        .set("status", status)
                        .set("finishedAt", finished_at.clone()),
                )?;
            }
        }
        Ok(())
    }

    /// Begin finishing a parked session without calling the provider. The returned live session
    /// is marked in-flight and must be driven outside the manager lock, then returned through
    /// [`Self::apply_finish_turn`].
    pub fn begin_finish(&mut self, run_id: &str) -> io::Result<FinishStart> {
        let Some(active) = self.active.remove(run_id) else {
            if self
                .get_run(run_id)
                .is_some_and(|run| run.status == RunStatus::Review)
            {
                self.update_run(run_id, RunPatch::new().set("status", RunStatus::Done))?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle")
                        .field("message", "review accepted — finished without a PR"),
                )?;
                self.cleanup_runtime(run_id);
                self.pump()?;
                return Ok(FinishStart::Finished(true));
            }
            if self
                .get_run(run_id)
                .is_some_and(|run| run.status == RunStatus::Waiting)
            {
                self.settle_steps(run_id, StepStatus::Done)?;
                self.settle_success(run_id)?;
                self.pump()?;
                return Ok(FinishStart::Finished(true));
            }
            return Ok(FinishStart::Finished(false));
        };
        self.in_flight.insert(run_id.to_owned());
        Ok(FinishStart::Detached(Box::new(active)))
    }

    /// Apply the result of a detached user-requested finish. Waiting/running provider outcomes
    /// are coerced to completion because the user explicitly closed the session.
    pub fn apply_finish_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        finish_result: Result<SessionOutcome, String>,
    ) -> io::Result<TurnStep> {
        let outcome = match finish_result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.in_flight.remove(run_id);
                self.active.insert(run_id.to_owned(), active);
                self.append_event(
                    run_id,
                    EventInput::new("error")
                        .field("message", format!("could not finish session: {error}")),
                )?;
                return Ok(TurnStep::Done);
            }
        };
        if let Err(error) = self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", "session closed by user"),
        ) {
            self.in_flight.remove(run_id);
            self.active.insert(run_id.to_owned(), active);
            return Err(error);
        }
        let outcome = match outcome {
            SessionOutcome::Running(report) | SessionOutcome::Waiting(report) => {
                SessionOutcome::Completed(report)
            }
            other => other,
        };
        self.apply_active_turn(run_id, active, Ok(outcome), false)
    }

    #[cfg(test)]
    pub fn finish(&mut self, run_id: &str) -> io::Result<bool> {
        let mut active = match self.begin_finish(run_id)? {
            FinishStart::Finished(finished) => return Ok(finished),
            FinishStart::Detached(active) => *active,
        };
        let step_id = active.step_id().to_owned();
        let result = active
            .session_mut()
            .finish(&mut self.event_sink(run_id, &step_id));
        let mut step = self.apply_finish_turn(run_id, active, result)?;
        loop {
            match step {
                TurnStep::Done => return Ok(true),
                TurnStep::GitAutoCommit(_) => {
                    self.finish_git_auto(
                        run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                    return Ok(true);
                }
                TurnStep::Nudge(mut active) => {
                    let step_id = active.step_id().to_owned();
                    let result = active.session_mut().send_message(
                        AUTONOMOUS_NUDGE,
                        &[],
                        &mut self.event_sink(run_id, &step_id),
                    );
                    step = self.apply_active_turn(run_id, *active, result, false)?;
                }
            }
        }
    }

    /// Drive a synchronously resumed session in core unit tests through
    /// `continue_active_turn` to completion, sending any autonomous nudge inline under the same
    /// test-owned manager borrow. Production never compiles this path; it uses `TurnDispatch`.
    #[cfg(test)]
    fn drive_active_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        outcome: SessionOutcome,
    ) -> io::Result<()> {
        let mut step = self.continue_active_turn(run_id, active, outcome)?;
        loop {
            let TurnStep::Nudge(boxed) = step else {
                if matches!(step, TurnStep::GitAutoCommit(_)) {
                    self.finish_git_auto(
                        run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                }
                break;
            };
            let mut active = *boxed;
            let step_id = active.workflow.steps[active.step_index].id.clone();
            let next = match active.session_mut().send_message(
                AUTONOMOUS_NUDGE,
                &[],
                &mut self.event_sink(run_id, &step_id),
            ) {
                Ok(outcome) => outcome,
                Err(error) => SessionOutcome::Failed {
                    message: error,
                    report: SessionReport::default(),
                },
            };
            step = self.continue_active_turn(run_id, active, next)?;
        }
        Ok(())
    }

    /// Synchronous convenience used only by core unit tests. Production detaches through
    /// [`Self::begin_message`] and drives the provider on `TurnDispatch`.
    #[cfg(test)]
    pub fn send_message(&mut self, run_id: &str, prompt: impl Into<String>) -> io::Result<bool> {
        self.deliver_message(run_id, prompt, Vec::new())
    }

    /// Synchronous backend-neutral delivery seam used only by core unit tests.
    #[cfg(test)]
    pub fn deliver_message(
        &mut self,
        run_id: &str,
        prompt: impl Into<String>,
        images: Vec<PromptImage>,
    ) -> io::Result<bool> {
        let prompt = prompt.into();
        let Some(active) = self.active.get(run_id) else {
            return Ok(false);
        };
        let step_id = active.workflow.steps[active.step_index].id.clone();
        self.append_user_message(run_id, &step_id, &prompt, &images)?;
        let Some(mut active) = self.active.remove(run_id) else {
            return Ok(false);
        };
        let send_result =
            active
                .session
                .send_message(&prompt, &images, &mut self.event_sink(run_id, &step_id));
        let outcome = match send_result {
            Ok(outcome) => outcome,
            Err(_) => {
                self.active.insert(run_id.to_owned(), active);
                return Ok(false);
            }
        };
        self.drive_active_turn(run_id, active, outcome)?;
        Ok(true)
    }

    /// Durably append a follow-up and detach its parked session without calling the provider.
    /// The caller owns the returned session until it reports the outcome through
    /// [`Self::apply_active_turn`]. No active session means `continue_run` is the valid path.
    pub fn begin_message(
        &mut self,
        run_id: &str,
        prompt: impl Into<String>,
        images: Vec<PromptImage>,
    ) -> io::Result<Option<RuntimeActive>> {
        let prompt = prompt.into();
        let Some(active) = self.active.get(run_id) else {
            return Ok(None);
        };
        let step_id = active.step_id().to_owned();
        self.append_user_message(run_id, &step_id, &prompt, &images)?;
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Running)
                .clear("activity"),
        )?;
        self.update_step(
            run_id,
            &step_id,
            StepPatch::new().set("status", StepStatus::Running),
        )?;
        let Some(active) = self.active.remove(run_id) else {
            return Ok(None);
        };
        self.in_flight.insert(run_id.to_owned());
        Ok(Some(active))
    }

    /// Apply a detached user follow-up while preserving the parked session on a provider-level
    /// delivery error. Cancellation still wins and follows the ordinary cancelled-turn path.
    pub fn apply_message_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        send_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let outcome = match send_result {
            Ok(outcome) => outcome,
            Err(error) if cancellation_requested => {
                return self.apply_active_turn(run_id, active, Err(error), true);
            }
            Err(error) => {
                self.in_flight.remove(run_id);
                let step_id = active.step_id().to_owned();
                self.update_run(run_id, RunPatch::new().set("status", RunStatus::Waiting))?;
                self.update_step(
                    run_id,
                    &step_id,
                    StepPatch::new().set("status", StepStatus::Waiting),
                )?;
                self.active.insert(run_id.to_owned(), active);
                self.append_event(
                    run_id,
                    EventInput::new("error")
                        .field("message", format!("could not deliver follow-up: {error}")),
                )?;
                return Ok(TurnStep::Done);
            }
        };
        self.apply_active_turn(run_id, active, Ok(outcome), cancellation_requested)
    }

    /// Put a detached parked session back if the OS could not start its worker. The durable user
    /// message remains in history, and the run remains available for retry or cancellation.
    pub fn abandon_message(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        let running = active.holds_slot;
        let _ = self.update_run(
            run_id,
            RunPatch::new().set(
                "status",
                if running {
                    RunStatus::Running
                } else {
                    RunStatus::Waiting
                },
            ),
        );
        let _ = self.update_step(
            run_id,
            active.step_id(),
            StepPatch::new().set(
                "status",
                if running {
                    StepStatus::Running
                } else {
                    StepStatus::Waiting
                },
            ),
        );
        self.active.insert(run_id.to_owned(), active);
    }

    /// Reattach a detached session whose finish worker could not be created. Finishing does not
    /// change the durable status before dispatch, so no status rollback is required.
    pub fn abandon_finish(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        self.active.insert(run_id.to_owned(), active);
    }

    /// Reopen the last persisted session as a new synthetic step. Overrides are written before
    /// queueing so a later continuation and the cockpit both see the selected runner/model.
    pub fn continue_run(
        &mut self,
        run_id: &str,
        options: ContinueOptions,
    ) -> io::Result<ContinueResult> {
        if self.active.contains_key(run_id) {
            return Ok(ContinueResult::error("run is still active"));
        }
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(ContinueResult::error("not found"));
        };
        if !matches!(
            run.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review
        ) {
            return Ok(ContinueResult::error(format!(
                "cannot continue a {} run",
                run_status_name(run.status)
            )));
        }
        // A run with no prior session (e.g. the agent crashed before its first turn) still gets
        // a fresh step in this same run/worktree — it just starts without a resumed transcript,
        // the same as a resumed step whose backend no longer matches the target runner below.
        let session_step = run
            .steps
            .iter()
            .rev()
            .find(|step| step.session_id.is_some());
        let target_runner = options
            .runner
            .unwrap_or(run.requested_runner.unwrap_or_else(|| {
                run.runner
                    .map(runner_selection)
                    .unwrap_or(RunnerSelection::Claude)
            }));
        if target_runner == RunnerSelection::Auto
            && options
                .model
                .as_deref()
                .is_some_and(|model| !model.is_empty())
        {
            return Ok(ContinueResult::error(
                "a model override cannot be used with quota-aware routing",
            ));
        }
        let target_concrete = concrete_runner(target_runner);
        if let (Some(model), Some(runner)) = (options.model.as_deref(), target_concrete)
            && model_conflicts_with_runner(model, runner)
        {
            return Ok(ContinueResult::error(format!(
                "model '{model}' is not a {} model",
                runner_name(runner)
            )));
        }
        let Some(workflow) = run.workflow_def.clone() else {
            return Ok(ContinueResult::error(
                "workflow definition not recoverable for continuation",
            ));
        };
        let session_backend = session_step
            .and_then(|step| step.backend)
            .or(run.runner)
            .unwrap_or(Runner::Claude);
        let prior_session_id = session_step.and_then(|step| step.session_id.clone());
        let resume_session = (target_concrete == Some(session_backend))
            .then(|| prior_session_id.clone())
            .flatten();
        // A real prior session exists but the runner switch makes it unresumable: the new step
        // starts with an empty transcript instead of the one the user was just looking at. Say so
        // — silently dropping the conversation is exactly the kind of quiet capability loss
        // CODE_REVIEW.md rules out; the user should see this in the same place they'd see any
        // other run note, not have to notice a step counter or discover it from a confused reply.
        if prior_session_id.is_some() && resume_session.is_none() {
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "switching from {} to {} starts a fresh session — the previous conversation is not resumed",
                        runner_name(session_backend),
                        runner_name(target_concrete.unwrap_or(Runner::Claude)),
                    ),
                ),
            )?;
        }

        if options.runner.is_some() || options.model.is_some() {
            let mut patch = RunPatch::new();
            if let Some(runner) = options.runner {
                patch = patch.set("requestedRunner", runner);
                if runner != RunnerSelection::Auto {
                    patch = patch.set("runner", runner);
                }
            }
            if let Some(model) = options.model {
                if model.is_empty() {
                    patch = patch.clear("model");
                } else {
                    patch = patch.set("model", model);
                }
            } else if options.runner.is_some()
                && (target_runner == RunnerSelection::Auto
                    || run
                        .model
                        .as_deref()
                        .zip(target_concrete)
                        .is_some_and(|(model, runner)| model_conflicts_with_runner(model, runner)))
            {
                patch = patch.clear("model");
            }
            self.update_run(run_id, patch)?;
        }

        let count = run
            .steps
            .iter()
            .filter(|step| step.id.starts_with("continue-"))
            .count();
        let step_id = format!("continue-{}", count + 1);
        self.add_step(
            run_id,
            StepSeed {
                id: step_id.clone(),
                name: "Continue".to_owned(),
                kind: StepKind::Agent,
                requested_runner: Some(target_runner),
            },
        )?;
        let prompt = options
            .text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| {
                if options.images.is_empty() {
                    "Continue.".to_owned()
                } else {
                    String::new()
                }
            });
        self.append_user_message(run_id, &step_id, &prompt, &options.images)?;
        let model = self.get_run(run_id).and_then(|run| run.model.clone());
        let mut continuation_workflow = workflow;
        continuation_workflow
            .steps
            .push(coducktor_contract::workflows::WorkflowStepDef {
                id: step_id.clone(),
                name: Some("Continue".to_owned()),
                prompt: Some("{{task}}".to_owned()),
                skill: None,
                model: None,
                runner: Some(target_runner),
                allowed_tools: None,
                bash_allowlist: None,
                command: None,
                on_fail: None,
            });
        self.update_run(
            run_id,
            RunPatch::new()
                .set("workflowDef", &continuation_workflow)
                .set("status", RunStatus::Queued)
                .clear("error")
                .clear("finishedAt")
                .clear("currentStepId")
                .set("requestedRunner", target_runner),
        )?;
        let step_index = continuation_workflow.steps.len().saturating_sub(1);
        self.jobs.insert(
            run_id.to_owned(),
            RuntimeJob::Continuation {
                workflow: continuation_workflow,
                step_index,
                session_id: resume_session,
                prompt,
                images: options.images,
                runner: target_runner,
                model,
                retry_counts: BTreeMap::new(),
            },
        );
        self.enqueue(run_id.to_owned());
        self.pump()?;
        Ok(ContinueResult::ok())
    }

    fn append_user_message(
        &mut self,
        run_id: &str,
        step_id: &str,
        prompt: &str,
        images: &[PromptImage],
    ) -> io::Result<RunEvent> {
        let image_urls = images.iter().map(PromptImage::data_url).collect::<Vec<_>>();
        self.append_event(
            run_id,
            EventInput::new("user-message")
                .step(step_id.to_owned())
                .field("text", prompt)
                .field("imageCount", image_urls.len())
                .field("images", image_urls),
        )
    }

    /// Apply parsed agent-owned PR/issue markers to a record. URL candidate discovery remains a
    /// separate session concern; this method preserves the authoritative marker fields and
    /// resolves any candidate set already present on the record.
    pub fn apply_marker_refs(
        &mut self,
        run_id: &str,
        refs: &TaskMarkers,
    ) -> io::Result<Option<RunRecord>> {
        if refs.pr.is_none() && refs.issue.is_none() {
            return Ok(self.get_run(run_id).cloned());
        }
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        let mut marker_refs = next.marker_refs.take().unwrap_or(MarkerRefs {
            pr: None,
            issue: None,
        });
        if let Some(pr) = refs.pr {
            marker_refs.pr = Some(pr as f64);
            next.pr_number = Some(pr as f64);
            next.referenced_pull_request_url = resolve_reference(
                next.referenced_pr_candidates.as_deref().unwrap_or(&[]),
                &next.task,
                Some(pr),
            );
        }
        if let Some(issue) = refs.issue {
            marker_refs.issue = Some(issue as f64);
            next.issue_number = Some(issue as f64);
            next.referenced_issue_number_seeded = None;
            next.referenced_issue_url = resolve_reference(
                next.referenced_issue_candidates.as_deref().unwrap_or(&[]),
                &next.task,
                Some(issue),
            );
        }
        next.marker_refs = Some(marker_refs);
        self.replace_record(run_id, previous, next).map(Some)
    }

    /// Parse and apply the reference/title markers from one completed turn. This deliberately
    /// reads only the supplied agent text; tool output should call `append_event`, not this helper.
    pub fn apply_turn_markers(
        &mut self,
        run_id: &str,
        turn_text: &str,
    ) -> io::Result<Option<RunRecord>> {
        let markers = task_markers::parse_task_markers(turn_text);
        let _ = self.apply_marker_refs(run_id, &markers)?;
        let Some(current) = self.get_run(run_id).cloned() else {
            return Ok(None);
        };
        let Some(title) = markers.title.as_deref() else {
            return Ok(Some(current));
        };
        if current.title_origin == Some(coducktor_contract::runs::TitleOrigin::User) {
            return Ok(Some(current));
        }
        let ref_number = current
            .pr_number
            .or(current.issue_number)
            .map(|number| number as i64);
        let Some(title) = post_validate_marker_title(title, ref_number) else {
            return Ok(Some(current));
        };
        let patch = RunPatch::new()
            .set("titleSummary", title)
            .set("titleOrigin", coducktor_contract::runs::TitleOrigin::Marker);
        self.update_run(run_id, patch)
    }
}

/// `Auto` has no single concrete level here; `None` lets the backend fall back to its own default.
fn concrete_reasoning_effort(effort: ReasoningEffort) -> Option<ConcreteReasoningEffort> {
    match effort {
        ReasoningEffort::Auto => None,
        ReasoningEffort::Low => Some(ConcreteReasoningEffort::Low),
        ReasoningEffort::Medium => Some(ConcreteReasoningEffort::Medium),
        ReasoningEffort::High => Some(ConcreteReasoningEffort::High),
        ReasoningEffort::XHigh => Some(ConcreteReasoningEffort::XHigh),
    }
}

fn concrete_runner(selection: RunnerSelection) -> Option<Runner> {
    match selection {
        RunnerSelection::Claude => Some(Runner::Claude),
        RunnerSelection::Codex => Some(Runner::Codex),
        RunnerSelection::OpenCode => Some(Runner::OpenCode),
        RunnerSelection::Pi => Some(Runner::Pi),
        RunnerSelection::Auto => None,
    }
}

fn runner_selection(runner: Runner) -> RunnerSelection {
    match runner {
        Runner::Claude => RunnerSelection::Claude,
        Runner::Codex => RunnerSelection::Codex,
        Runner::OpenCode => RunnerSelection::OpenCode,
        Runner::Pi => RunnerSelection::Pi,
    }
}

fn run_status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Waiting => "waiting",
        RunStatus::Review => "review",
        RunStatus::Done => "done",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn model_conflicts_with_runner(model: &str, runner: Runner) -> bool {
    if model.is_empty() {
        return false;
    }
    let own = match runner {
        Runner::Claude => [
            "opus",
            "sonnet",
            "haiku",
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-haiku-4-5",
        ]
        .as_slice(),
        Runner::Codex => ["gpt-5.1-codex", "gpt-5.1-codex-mini", "gpt-5-codex"].as_slice(),
        Runner::OpenCode | Runner::Pi => &[],
    };
    if own.contains(&model) {
        return false;
    }
    let known = [
        "opus",
        "sonnet",
        "haiku",
        "claude-fable-5",
        "claude-opus-4-8",
        "claude-sonnet-5",
        "claude-haiku-4-5",
        "gpt-5.1-codex",
        "gpt-5.1-codex-mini",
        "gpt-5-codex",
    ];
    known.contains(&model) && !own.contains(&model)
        || (matches!(runner, Runner::Codex | Runner::Pi)
            && model
                .split_once('/')
                .is_some_and(|(provider, _)| provider == "anthropic" || provider == "google"))
}

fn session_outcome_report(outcome: &SessionOutcome) -> &SessionReport {
    match outcome {
        SessionOutcome::Completed(report)
        | SessionOutcome::Running(report)
        | SessionOutcome::Waiting(report)
        | SessionOutcome::Cancelled(report) => report,
        SessionOutcome::Failed { report, .. } => report,
    }
}

fn apply_template(template: &str, task: &str) -> String {
    template.replace("{{task}}", task)
}

fn apply_run_patch(record: &RunRecord, patch: &Map<String, Value>) -> io::Result<RunRecord> {
    let value = serde_json::to_value(record).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("could not serialize run before patch: {error}"),
        )
    })?;
    let Some(mut object) = value.as_object().cloned() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "run record did not serialize as an object",
        ));
    };
    for (field, value) in patch {
        object.insert(field.clone(), value.clone());
    }
    let mut next: RunRecord = serde_json::from_value(Value::Object(object)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid run patch: {error}"),
        )
    })?;
    if patch.contains_key("issueNumber") {
        next.referenced_issue_number_seeded = None;
    }
    // These are transition rules, not record-wide invariants: a patch to a queued message must
    // not retire an unrelated scheduled resume or clear a provider block just because the record
    // currently happens to be queued.
    if patch.contains_key("status") {
        if matches!(
            next.status,
            RunStatus::Running | RunStatus::Waiting | RunStatus::Queued
        ) {
            next.auto_resume_at = None;
        } else {
            next.activity = None;
            next.monitoring_wake_at = None;
            next.monitoring_wake_cap_reached = None;
        }
        if next.status != RunStatus::Queued {
            next.blocked_reason = None;
        }
    }
    if next.archived {
        next.auto_resume_at = None;
        next.auto_resume_attempts = None;
    }
    Ok(next)
}

fn apply_step_patch(
    run: &mut RunRecord,
    step_index: usize,
    patch: &Map<String, Value>,
) -> io::Result<()> {
    let value = serde_json::to_value(&run.steps[step_index]).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("could not serialize step before patch: {error}"),
        )
    })?;
    let Some(mut object) = value.as_object().cloned() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "step state did not serialize as an object",
        ));
    };
    for (field, value) in patch {
        object.insert(field.clone(), value.clone());
    }
    run.steps[step_index] = serde_json::from_value(Value::Object(object)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid step patch: {error}"),
        )
    })?;
    Ok(())
}

fn recompute_aggregates(run: &mut RunRecord) {
    run.tokens_used = run.steps.iter().map(|step| step.tokens_used).sum();
    let started_agent_steps: Vec<&StepState> = run
        .steps
        .iter()
        .filter(|step| step.kind == StepKind::Agent && step.iterations > 0.0)
        .collect();
    let directional_complete = !started_agent_steps.is_empty()
        && started_agent_steps.iter().all(|step| {
            matches!(
                (
                    step.usage_invocations_started,
                    step.usage_invocations_observed,
                    step.usage_turns_started,
                    step.usage_turns_recorded,
                    step.input_tokens,
                    step.output_tokens,
                ),
                (
                    Some(invocations_started),
                    Some(invocations_observed),
                    Some(turns_started),
                    Some(turns_recorded),
                    Some(_),
                    Some(_),
                ) if invocations_observed > 0.0
                    && invocations_started == invocations_observed
                    && turns_started > 0.0
                    && turns_started == turns_recorded
            )
        });
    if directional_complete {
        run.input_tokens = Some(
            started_agent_steps
                .iter()
                .map(|step| step.input_tokens.unwrap_or(0.0))
                .sum(),
        );
        run.output_tokens = Some(
            started_agent_steps
                .iter()
                .map(|step| step.output_tokens.unwrap_or(0.0))
                .sum(),
        );
    } else {
        run.input_tokens = None;
        run.output_tokens = None;
    }
    let cost: f64 = run
        .steps
        .iter()
        .map(|step| step.cost_usd.unwrap_or(0.0))
        .sum();
    run.cost_usd = (cost > 0.0).then_some(cost);
}

fn step_from_seed(seed: StepSeed) -> StepState {
    StepState {
        id: seed.id,
        name: seed.name,
        kind: seed.kind,
        status: StepStatus::Pending,
        iterations: 0.0,
        tokens_used: 0.0,
        input_tokens: None,
        output_tokens: None,
        usage_invocations_started: None,
        usage_invocations_observed: None,
        usage_turns_started: None,
        usage_turns_recorded: None,
        usage_invocation_epoch: None,
        started_at: None,
        finished_at: None,
        error: None,
        session_id: None,
        backend: None,
        requested_runner: seed.requested_runner,
        profile_id: None,
        reasoning_effort: None,
        cost_usd: None,
        model_identity: None,
        route_key: None,
        recovery_generation: None,
        routing_decision: None,
        extra: Map::new(),
    }
}

fn resolve_reference(candidates: &[String], task: &str, declared: Option<i64>) -> Option<String> {
    if let Some(declared) = declared {
        return candidates
            .iter()
            .find(|candidate| candidate_number(candidate) == Some(declared))
            .cloned();
    }
    if candidates.len() == 1 {
        return candidates.first().cloned();
    }
    let named: Vec<&String> = candidates
        .iter()
        .filter(|candidate| {
            candidate_number(candidate).is_some_and(|number| task_mentions_number(task, number))
        })
        .collect();
    (named.len() == 1).then(|| named[0].clone())
}

fn candidate_number(url: &str) -> Option<i64> {
    url.rsplit('/').next()?.parse().ok()
}

fn task_mentions_number(task: &str, number: i64) -> bool {
    task.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<i64>().ok())
        .any(|candidate| candidate == number)
}

/// Apply the marker title normalization used for task references.
pub fn post_validate_marker_title(title: &str, ref_number: Option<i64>) -> Option<String> {
    let mut normalized = title
        .trim()
        .trim_end_matches('.')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(number) = ref_number {
        let prefix = number.to_string();
        let without_hash = normalized.strip_prefix('#').unwrap_or(&normalized);
        if without_hash == prefix {
            normalized.clear();
        } else if let Some(rest) = without_hash.strip_prefix(&prefix) {
            let rest = rest.trim_start_matches([' ', ':', '-', '—']);
            normalized = rest.to_owned();
        }
    }
    if normalized.is_empty() {
        return None;
    }
    if let Some(first) = normalized.chars().next() {
        let lower = first.to_lowercase().collect::<String>();
        normalized.replace_range(..first.len_utf8(), &lower);
    }
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() > 40 {
        normalized = chars[..39].iter().collect::<String>() + "…";
    }
    Some(match ref_number {
        Some(number) => format!("{number}: {normalized}"),
        None => normalized,
    })
}

fn new_run_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("run-{nanos:x}-{counter:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::runs::{QueuedMessage, TitleOrigin};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    use super::semaphore::{RepositoryRootLease, WorkspaceSemaphore};

    #[test]
    fn retention_prunes_terminal_sidecars_but_keeps_live_records_and_worktrees() {
        let dir = tempdir().unwrap();
        let stale_id = "done-0000";
        let retained_worktree = dir.path().join("recoverable-worktree");
        fs::create_dir(&retained_worktree).unwrap();
        let mut records = Vec::new();
        for index in 0..=(store::MAX_RUNS_KEPT + 1) {
            records.push(RunRecord {
                id: format!("done-{index:04}"),
                created_at: format!("2026-01-{index:04}T00:00:00.000Z"),
                status: RunStatus::Done,
                worktree_path: (index == 0)
                    .then(|| retained_worktree.to_string_lossy().into_owned()),
                ..RunRecord::default()
            });
        }
        let live_id = "queued-old";
        records.push(RunRecord {
            id: live_id.to_owned(),
            created_at: "2000-01-01T00:00:00.000Z".to_owned(),
            status: RunStatus::Queued,
            ..RunRecord::default()
        });
        store::write_run_index(&store::index_path(dir.path()), &records).unwrap();

        fs::create_dir_all(dir.path().join("runs")).unwrap();
        fs::write(events::events_path(dir.path(), stale_id), "{}\n").unwrap();
        fs::write(
            crate::handoff::handoff_path(dir.path(), stale_id),
            "handoff",
        )
        .unwrap();

        let mut manager = RunManager::open(dir.path());
        let pruned = manager.prune_stale_runs().unwrap();

        assert!(pruned.iter().any(|id| id == stale_id));
        assert!(manager.get_run(stale_id).is_none());
        assert!(manager.get_run(live_id).is_some());
        assert!(!events::events_path(dir.path(), stale_id).exists());
        assert!(!crate::handoff::handoff_path(dir.path(), stale_id).exists());
        assert!(retained_worktree.is_dir());
        assert_eq!(manager.list_runs().len(), store::MAX_RUNS_KEPT + 1);
    }

    fn step(id: &str, kind: StepKind) -> StepSeed {
        StepSeed {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            requested_runner: None,
        }
    }

    fn create_input() -> CreateRunInput {
        CreateRunInput {
            title: "task".to_owned(),
            workflow: "quick-task".to_owned(),
            task: "task".to_owned(),
            steps: vec![step("work", StepKind::Agent)],
            ..CreateRunInput::default()
        }
    }

    #[test]
    fn truncated_index_quarantines_writes_until_an_explicit_repair_keeps_a_backup() {
        let dir = tempdir().unwrap();
        let index_path = store::index_path(dir.path());
        let truncated = b"[{\"id\": \"interrupted";
        fs::write(&index_path, truncated).unwrap();

        let mut manager = RunManager::open(dir.path());
        assert!(manager.list_runs().is_empty());
        let error = manager.create_run(create_input()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&index_path).unwrap(), truncated);

        let backup = manager.repair_quarantined_index().unwrap().unwrap();
        assert_eq!(fs::read(&backup).unwrap(), truncated);
        manager.create_run(create_input()).unwrap();
        assert_eq!(manager.list_runs().len(), 1);
    }

    #[test]
    fn create_update_add_and_update_step_are_durable() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        manager
            .update_run(
                &run.id,
                RunPatch::new().set("titleSummary", "A useful title"),
            )
            .unwrap();
        assert!(
            manager
                .add_step(&run.id, step("check", StepKind::Check))
                .unwrap()
        );
        assert!(
            !manager
                .add_step(&run.id, step("check", StepKind::Check))
                .unwrap()
        );
        manager
            .update_step(
                &run.id,
                "work",
                StepPatch::new()
                    .set("iterations", 1.0)
                    .set("tokensUsed", 12.0)
                    .set("costUsd", 0.25),
            )
            .unwrap();

        let reopened = RunManager::open(dir.path());
        let saved = reopened.get_run(&run.id).unwrap();
        assert_eq!(saved.title_summary.as_deref(), Some("A useful title"));
        assert_eq!(saved.steps.len(), 2);
        assert_eq!(saved.tokens_used, 12.0);
        assert_eq!(saved.cost_usd, Some(0.25));
    }

    #[test]
    fn directional_usage_deduplicates_turns_and_keeps_incomplete_aggregates_absent() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        manager
            .update_step(&run.id, "work", StepPatch::new().set("iterations", 1.0))
            .unwrap();
        manager.begin_usage_invocation(&run.id, "work").unwrap();
        manager
            .record_usage_event(
                &run.id,
                UsageEvent::TurnStarted {
                    turn_id: "turn-1".to_owned(),
                },
            )
            .unwrap();
        assert!(
            !manager
                .record_usage_event(
                    &run.id,
                    UsageEvent::TurnStarted {
                        turn_id: "turn-1".to_owned(),
                    },
                )
                .unwrap()
        );
        manager
            .record_usage_event(
                &run.id,
                UsageEvent::TurnCompleted {
                    turn_id: "turn-1".to_owned(),
                    input_tokens: Some(10.0),
                    output_tokens: Some(2.0),
                },
            )
            .unwrap();
        assert!(
            !manager
                .record_usage_event(
                    &run.id,
                    UsageEvent::TurnCompleted {
                        turn_id: "turn-1".to_owned(),
                        input_tokens: Some(10.0),
                        output_tokens: Some(2.0),
                    },
                )
                .unwrap()
        );
        manager
            .record_usage_event(
                &run.id,
                UsageEvent::TurnStarted {
                    turn_id: "turn-2".to_owned(),
                },
            )
            .unwrap();
        manager
            .record_usage_event(
                &run.id,
                UsageEvent::TurnCompleted {
                    turn_id: "turn-2".to_owned(),
                    input_tokens: Some(5.0),
                    output_tokens: Some(1.0),
                },
            )
            .unwrap();
        let saved = manager.get_run(&run.id).unwrap();
        assert_eq!(saved.steps[0].usage_invocations_started, Some(1.0));
        assert_eq!(saved.steps[0].usage_invocations_observed, Some(1.0));
        assert_eq!(saved.steps[0].usage_turns_started, Some(2.0));
        assert_eq!(saved.steps[0].usage_turns_recorded, Some(2.0));
        assert_eq!(saved.input_tokens, Some(15.0));
        assert_eq!(saved.output_tokens, Some(3.0));

        manager.begin_usage_invocation(&run.id, "work").unwrap();
        let incomplete = manager.get_run(&run.id).unwrap();
        assert_eq!(incomplete.input_tokens, None);
        assert_eq!(incomplete.output_tokens, None);
    }

    #[test]
    fn event_sequences_rehydrate_and_observers_see_ordered_events() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_by_callback = observed.clone();
        manager.subscribe_events(move |notification| {
            observed_by_callback
                .lock()
                .unwrap()
                .push(notification.event.seq);
        });
        assert_eq!(
            manager
                .append_event(&run.id, EventInput::new("note").field("message", "one"),)
                .unwrap()
                .seq,
            1.0
        );
        manager
            .append_event(&run.id, EventInput::new("note").field("message", "two"))
            .unwrap();
        assert_eq!(*observed.lock().unwrap(), vec![1.0, 2.0]);
        assert_eq!(
            manager.event_appenders.len(),
            1,
            "streamed events reuse one run-scoped append handle"
        );

        let reopened = RunManager::open(dir.path());
        let continued = reopened.read_events(&run.id);
        assert_eq!(
            continued.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [1.0, 2.0]
        );
        drop(reopened);
        let mut resumed = RunManager::open(dir.path());
        assert_eq!(
            resumed
                .append_event(&run.id, EventInput::new("note"))
                .unwrap()
                .seq,
            3.0
        );
    }

    #[test]
    fn streaming_events_debounce_run_index_notifications() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        let notifications = Arc::new(AtomicU64::new(0));
        let observed = notifications.clone();
        manager.subscribe_runs(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
        });
        let writes_before_stream = manager.index_write_count;
        for index in 0..10_000 {
            manager
                .append_event(
                    &run.id,
                    EventInput::new("text").field("text", format!("delta-{index}")),
                )
                .unwrap();
        }
        manager.flush().unwrap();
        let events = manager.read_events(&run.id);
        assert_eq!(events.len(), 10_000);
        // The exact final transcript, not just the right count: every delta present, in order,
        // with its own content intact — debounced index notifications must never coalesce away
        // or reorder the durable event log itself.
        assert!(
            events.windows(2).all(|pair| pair[0].seq < pair[1].seq),
            "events must stay in strictly increasing seq order"
        );
        for (index, event) in events.iter().enumerate() {
            assert_eq!(
                event.extra.get("text").and_then(Value::as_str),
                Some(format!("delta-{index}")).as_deref()
            );
        }
        let metrics = manager.runtime_metrics();
        assert_eq!(metrics.event_appends, 10_000);
        assert!(metrics.index_flushes >= 1);
        assert!(metrics.index_flush_bytes > 0);
        assert!(notifications.load(Ordering::Relaxed) < 100);
        assert!(manager.index_write_count - writes_before_stream < 100);
    }

    #[test]
    fn archive_and_read_receipts_match_the_finished_run_rules() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let done = manager.create_run(create_input()).unwrap();
        manager
            .update_run(
                &done.id,
                RunPatch::new()
                    .set("status", RunStatus::Done)
                    .set("finishedAt", "2020-01-01T00:00:00.000Z"),
            )
            .unwrap();
        let done_activity = manager.get_run(&done.id).unwrap().updated_at.clone();
        let scheduled = manager.create_run(create_input()).unwrap();
        manager
            .update_run(
                &scheduled.id,
                RunPatch::new()
                    .set("status", RunStatus::Failed)
                    .set("finishedAt", "2020-01-01T00:00:00.000Z")
                    .set("autoResumeAt", "2099-01-01T00:00:00.000Z"),
            )
            .unwrap();
        assert_eq!(manager.mark_all_read().unwrap(), 1);
        assert!(manager.get_run(&done.id).unwrap().seen_at.is_some());
        assert_eq!(manager.get_run(&done.id).unwrap().updated_at, done_activity);
        assert!(manager.get_run(&scheduled.id).unwrap().seen_at.is_none());
        manager.set_unread(&done.id).unwrap();
        assert_eq!(manager.mark_all_read().unwrap(), 1);
        manager.set_archived(&scheduled.id, true).unwrap();
        let archived = manager.get_run(&scheduled.id).unwrap();
        assert!(archived.archived);
        assert!(archived.archived_at.is_some());
        assert!(archived.auto_resume_at.is_none());
    }

    #[test]
    fn queue_helpers_keep_fifo_and_start_accounting_separate() {
        let mut queue = QueueState::default();
        assert!(queue.enqueue("a"));
        assert!(queue.enqueue("b"));
        assert!(!queue.enqueue("a"));
        assert_eq!(queue.take_next().as_deref(), Some("a"));
        assert!(queue.is_starting("a"));
        assert_eq!(queue.take_next().as_deref(), Some("b"));
        assert!(queue.finish_start("a"));
    }

    #[test]
    fn marker_and_review_decisions_preserve_precedence() {
        assert_eq!(
            decide_turn_marker("work\nDUCK:MONITORING", true, true),
            TurnMarkerDecision::Ask
        );
        assert_eq!(
            decide_turn_marker("work\nDUCK:DONE", true, true),
            TurnMarkerDecision::Done
        );
        assert_eq!(
            decide_turn_marker("work\nDUCK:MONITORING", true, false),
            TurnMarkerDecision::Monitoring
        );
        assert!(!review_gate_enabled(None, Some("true")));
        assert!(review_gate_enabled(None, Some("1")));
        assert!(!review_gate_enabled(Some(false), Some("1")));
        assert_eq!(success_status(true, true, false), RunStatus::Review);
        assert_eq!(success_status(true, true, true), RunStatus::Done);
        assert_eq!(success_status(false, true, false), RunStatus::Done);
    }

    #[test]
    fn queued_hydration_is_read_only_and_trims_each_part() {
        let run = RunRecord {
            task: "  original task  ".to_owned(),
            queued_messages: Some(vec![
                QueuedMessage {
                    id: "m1".to_owned(),
                    text: "  first update ".to_owned(),
                    images: None,
                    created_at: "2026-01-01T00:00:00.000Z".to_owned(),
                },
                QueuedMessage {
                    id: "m2".to_owned(),
                    text: "  ".to_owned(),
                    images: None,
                    created_at: "2026-01-01T00:00:00.000Z".to_owned(),
                },
            ]),
            ..RunRecord::default()
        };
        assert_eq!(hydrate_queued_prompt(&run), "original task\n\nfirst update");
        assert_eq!(run.task, "  original task  ");
    }

    #[test]
    fn account_holds_allow_resumes_through_in_flight_but_not_deadline_holds() {
        let mut run = RunRecord {
            runner: Some(Runner::Claude),
            agent_profile: Some("work".to_owned()),
            status: RunStatus::Queued,
            auto_resume_attempts: Some(1.0),
            ..RunRecord::default()
        };
        let key = run_account_key(&run, Runner::Codex);
        let holds = AccountHolds {
            deadline: BTreeSet::new(),
            in_flight: [key.clone()].into_iter().collect(),
        };
        assert!(!account_held_for(&run, &holds, Runner::Codex));
        run.auto_resume_attempts = None;
        assert!(account_held_for(&run, &holds, Runner::Codex));
        let deadline = AccountHolds {
            deadline: [key].into_iter().collect(),
            in_flight: BTreeSet::new(),
        };
        assert!(account_held_for(&run, &deadline, Runner::Codex));
    }

    #[test]
    fn marker_refs_and_marker_titles_are_authoritative_but_user_titles_win() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        manager
            .apply_turn_markers(
                &run.id,
                "progress\nDUCK:PR=500\nDUCK:TITLE=Implementing comment threads",
            )
            .unwrap();
        let marked = manager.get_run(&run.id).unwrap();
        assert_eq!(marked.pr_number, Some(500.0));
        assert_eq!(
            marked.marker_refs.as_ref().and_then(|refs| refs.pr),
            Some(500.0)
        );
        assert_eq!(
            marked.title_summary.as_deref(),
            Some("500: implementing comment threads")
        );
        assert_eq!(marked.title_origin, Some(TitleOrigin::Marker));
        manager
            .update_run(
                &run.id,
                RunPatch::new()
                    .set("titleSummary", "My title")
                    .set("titleOrigin", TitleOrigin::User),
            )
            .unwrap();
        manager
            .apply_turn_markers(&run.id, "DUCK:PR=501\nDUCK:TITLE=other title")
            .unwrap();
        let user_owned = manager.get_run(&run.id).unwrap();
        assert_eq!(user_owned.title_summary.as_deref(), Some("My title"));
        assert_eq!(user_owned.pr_number, Some(501.0));
    }

    #[test]
    fn workflow_creation_reuses_the_shared_definition_and_step_kinds() {
        let workflow = WorkflowDef {
            name: "review".to_owned(),
            description: None,
            steps: vec![coducktor_contract::workflows::WorkflowStepDef {
                id: "check".to_owned(),
                name: None,
                prompt: None,
                skill: None,
                model: None,
                runner: None,
                allowed_tools: None,
                bash_allowlist: None,
                command: Some("true".to_owned()),
                on_fail: None,
            }],
            source: coducktor_contract::workflows::WorkflowSource::File,
            path: None,
        };
        let input = CreateRunInput::from_workflow(&workflow, "do it");
        assert_eq!(input.workflow_def, Some(workflow));
        assert_eq!(input.steps[0].kind, StepKind::Check);
    }

    #[test]
    fn json_patch_rejects_invalid_contract_values_without_losing_the_record() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        let error = manager
            .update_run_value(&run.id, json!({ "status": "not-a-status" }))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Queued);
    }

    struct FakeSession {
        outcome: Option<SessionOutcome>,
        follow_up: Option<SessionOutcome>,
    }

    /// Emits the outcome's raw `turn_text` as one `text` event — a real backend would call
    /// `on_event` per chunk as its process streams; the fake collapses that to a single call,
    /// which is enough to exercise `event_sink`'s per-chunk marker stripping (the sink, not the
    /// fake, is what strips it) without every test needing to script a whole fake stream.
    fn emit_fake_text(
        outcome: &SessionOutcome,
        on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
    ) -> Result<(), String> {
        let text = &session_outcome_report(outcome).turn_text;
        if !text.is_empty() {
            on_event(EventInput::new("text").field("text", text.clone()))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    impl AgentSession for FakeSession {
        fn turn(
            &mut self,
            on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
        ) -> Result<SessionOutcome, String> {
            let outcome = self
                .outcome
                .take()
                .ok_or_else(|| "fake session has no outcome".to_owned())?;
            emit_fake_text(&outcome, on_event)?;
            Ok(outcome)
        }

        fn send_message(
            &mut self,
            _prompt: &str,
            _images: &[PromptImage],
            on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
        ) -> Result<SessionOutcome, String> {
            let outcome = self
                .follow_up
                .take()
                .ok_or_else(|| "fake session declined follow-up".to_owned())?;
            emit_fake_text(&outcome, on_event)?;
            Ok(outcome)
        }
    }

    struct FakeFactory {
        outcomes: Arc<Mutex<std::collections::VecDeque<SessionOutcome>>>,
        requests: Arc<Mutex<Vec<SessionRequest>>>,
        follow_ups: Arc<Mutex<std::collections::VecDeque<SessionOutcome>>>,
    }

    impl SessionFactory for FakeFactory {
        fn open(&self, request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String> {
            self.requests.lock().unwrap().push(request);
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake factory ran out of outcomes".to_owned())?;
            let follow_up = self.follow_ups.lock().unwrap().pop_front();
            Ok(Box::new(FakeSession {
                outcome: Some(outcome),
                follow_up,
            }))
        }
    }

    struct FakeChecks {
        results: Arc<Mutex<std::collections::VecDeque<CheckResult>>>,
    }

    impl CheckExecutor for FakeChecks {
        fn run(&mut self, _command: &str, _cwd: &Path) -> Result<CheckResult, String> {
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake check executor ran out of results".to_owned())
        }
    }

    struct FakeDiff(bool);

    impl DiffInspector for FakeDiff {
        fn has_diff(&mut self, _run: &RunRecord) -> bool {
            self.0
        }
    }

    struct RecordingWorkspaceSemaphore {
        acquired: Arc<Mutex<Vec<(String, String)>>>,
        released: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl WorkspaceSemaphore for RecordingWorkspaceSemaphore {
        fn try_acquire(&mut self, run_id: &str, project_id: &str) -> bool {
            self.acquired
                .lock()
                .unwrap()
                .push((run_id.to_owned(), project_id.to_owned()));
            true
        }

        fn release(&mut self, run_id: &str, project_id: &str) {
            self.released
                .lock()
                .unwrap()
                .push((run_id.to_owned(), project_id.to_owned()));
        }

        fn busy_slots(&self) -> usize {
            0
        }

        fn max_parallel(&self) -> usize {
            1
        }
    }

    struct RecordingRepositoryLease {
        acquired: Arc<Mutex<Vec<String>>>,
        released: Arc<Mutex<Vec<String>>>,
    }

    impl RepositoryRootLease for RecordingRepositoryLease {
        fn try_acquire(&mut self, run_id: &str) -> bool {
            self.acquired.lock().unwrap().push(run_id.to_owned());
            true
        }

        fn release(&mut self, run_id: &str) {
            self.released.lock().unwrap().push(run_id.to_owned());
        }
    }

    fn fake_factory(
        outcomes: Vec<SessionOutcome>,
    ) -> (FakeFactory, Arc<Mutex<Vec<SessionRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory = FakeFactory {
            outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
            requests: requests.clone(),
            follow_ups: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        };
        (factory, requests)
    }

    fn fake_factory_with_followups(
        outcomes: Vec<SessionOutcome>,
        follow_ups: Vec<SessionOutcome>,
    ) -> (FakeFactory, Arc<Mutex<Vec<SessionRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory = FakeFactory {
            outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
            requests: requests.clone(),
            follow_ups: Arc::new(Mutex::new(follow_ups.into_iter().collect())),
        };
        (factory, requests)
    }

    fn completed_session(session_id: &str) -> SessionOutcome {
        SessionOutcome::Completed(SessionReport {
            session_id: Some(session_id.to_owned()),
            tokens_used: 3.0,
            cost_usd: Some(0.25),
            ..SessionReport::default()
        })
    }

    fn running_session() -> SessionOutcome {
        SessionOutcome::Running(SessionReport::default())
    }

    fn waiting_session(decision: Option<TurnMarkerDecision>) -> SessionOutcome {
        SessionOutcome::Waiting(SessionReport {
            decision,
            ..SessionReport::default()
        })
    }

    fn cancelled_session() -> SessionOutcome {
        SessionOutcome::Cancelled(SessionReport::default())
    }

    fn failed_session(message: &str) -> SessionOutcome {
        SessionOutcome::Failed {
            message: message.to_owned(),
            report: SessionReport::default(),
        }
    }

    fn workflow_with_steps(
        steps: Vec<coducktor_contract::workflows::WorkflowStepDef>,
    ) -> WorkflowDef {
        WorkflowDef {
            name: "test-workflow".to_owned(),
            description: None,
            steps,
            source: coducktor_contract::workflows::WorkflowSource::BuiltIn,
            path: None,
        }
    }

    fn agent_workflow_step(id: &str) -> coducktor_contract::workflows::WorkflowStepDef {
        coducktor_contract::workflows::WorkflowStepDef {
            id: id.to_owned(),
            name: Some(id.to_owned()),
            prompt: Some("{{task}}".to_owned()),
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        }
    }

    fn check_workflow_step(
        id: &str,
        retry: Option<&str>,
        max: u32,
    ) -> coducktor_contract::workflows::WorkflowStepDef {
        coducktor_contract::workflows::WorkflowStepDef {
            id: id.to_owned(),
            name: Some(id.to_owned()),
            prompt: None,
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: Some("verify".to_owned()),
            on_fail: retry.map(|retry| coducktor_contract::workflows::WorkflowOnFail {
                retry: retry.to_owned(),
                max,
            }),
        }
    }

    fn start_input(task: &str) -> StartRunInput {
        StartRunInput {
            task: task.to_owned(),
            runner: Some(RunnerSelection::Claude),
            ..StartRunInput::default()
        }
    }

    #[test]
    fn an_auto_run_persists_its_routing_decision_and_announces_it() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("auto-session")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let decision = RoutingDecision {
            selected: Some(coducktor_contract::RouteSelection {
                runner: Runner::Codex,
                profile_id: "default".to_owned(),
                upstream_provider: None,
                model: None,
                reasoning_effort: None,
                route_key: "codex:default".to_owned(),
            }),
            considered: vec![
                coducktor_contract::ConsideredCandidate {
                    route_key: "codex:default".to_owned(),
                    runner: Runner::Codex,
                    profile_id: "default".to_owned(),
                    model: None,
                    eligible: true,
                    reason: coducktor_contract::RoutingReasonCode::Selected,
                    score: Some(2),
                },
                coducktor_contract::ConsideredCandidate {
                    route_key: "claude:default".to_owned(),
                    runner: Runner::Claude,
                    profile_id: "default".to_owned(),
                    model: None,
                    eligible: false,
                    reason: coducktor_contract::RoutingReasonCode::ReservedQuota,
                    score: None,
                },
            ],
            retry_at: None,
            generation: 0,
        };
        let mut input = start_input("pick the best runner");
        input.runner = Some(RunnerSelection::Auto);
        input.resolved_runner = Some(Runner::Codex);
        input.auto_runner_candidates = vec![Runner::Codex];
        input.routing_decision = Some(decision.clone());

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(
            run.steps[0].routing_decision.as_ref(),
            Some(&decision),
            "the decision that produced resolved_runner is durably attached to the first step"
        );
        assert!(manager.read_events(&run.id).iter().any(|event| {
            event.event_type == "note"
                && event.extra.get("message").and_then(Value::as_str)
                    == Some("Auto routing · selected Codex\n  Claude — reserved quota")
        }));
        assert!(
            manager.read_events(&run.id).iter().any(|event| {
                event.event_type == "routing-decision"
                    && event.extra.get("decision")
                        == Some(&serde_json::to_value(&decision).unwrap())
            }),
            "the full structured decision is also durably persisted as its own event"
        );
    }

    #[test]
    fn runtime_executes_agent_and_check_steps_and_persists_session_usage() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-1")]);
        let checks = Arc::new(Mutex::new(std::collections::VecDeque::from([
            CheckResult {
                success: true,
                exit_code: 0,
                output: "ok".to_owned(),
            },
        ])));
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_check_executor(FakeChecks { results: checks });
        let workflow = workflow_with_steps(vec![
            agent_workflow_step("implement"),
            check_workflow_step("verify", None, 0),
        ]);

        let run = manager
            .start_run(&workflow, start_input("ship it"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(
            run.steps.iter().map(|step| step.status).collect::<Vec<_>>(),
            [StepStatus::Done, StepStatus::Done,]
        );
        assert_eq!(run.steps[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(run.steps[0].tokens_used, 3.0);
        assert_eq!(run.steps[0].cost_usd, Some(0.25));
        assert_eq!(run.tokens_used, 3.0);
        assert_eq!(requests.lock().unwrap()[0].prompt, "ship it");
        assert!(manager.active.is_empty());
        assert!(manager.jobs.is_empty());
    }

    #[test]
    fn enqueue_returns_the_durable_run_before_opening_an_agent_session() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-1")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("implement")]);

        let queued = manager
            .enqueue_run(&workflow, start_input("show activity immediately"))
            .unwrap();

        assert_eq!(queued.status, RunStatus::Queued);
        assert_eq!(queued.task, "show activity immediately");
        assert!(requests.lock().unwrap().is_empty());

        manager.run_to_completion().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn authored_auto_uses_the_resolved_provider_and_preserves_the_request() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-auto")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("implement")]);
        let mut input = start_input("ship it");
        input.runner = Some(RunnerSelection::Auto);
        input.resolved_runner = Some(Runner::OpenCode);

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(run.requested_runner, Some(RunnerSelection::Auto));
        assert_eq!(run.runner, Some(Runner::OpenCode));
        assert_eq!(
            requests.lock().unwrap()[0].runner,
            RunnerSelection::OpenCode
        );
    }

    #[test]
    fn session_request_carries_cwd_tools_prompt_and_reasoning_for_the_factory() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-1")]);
        let mut manager = RunManager::with_session_factory_for_repo(
            dir.path(),
            dir.path().join("state"),
            factory,
        );
        let mut step = agent_workflow_step("implement");
        step.allowed_tools = Some(vec!["Read".to_owned(), "Bash".to_owned()]);
        step.bash_allowlist = Some(vec!["npm test".to_owned()]);
        let workflow = workflow_with_steps(vec![step]);
        let mut input = start_input("ship it");
        input.system_prompt = Some("Stay focused.".to_owned());
        input.reasoning_effort = Some(ReasoningEffort::High);

        manager.start_run(&workflow, input).unwrap();

        let requests = requests.lock().unwrap();
        let expected_system_prompt = format!(
            "Stay focused.\n\n---\n\n{}",
            session::TASK_CONTROL_INSTRUCTIONS
        );
        assert_eq!(requests[0].cwd, dir.path());
        assert_eq!(requests[0].allowed_tools, vec!["Read", "Bash"]);
        assert_eq!(requests[0].bash_allowlist, vec!["npm test"]);
        assert_eq!(
            requests[0].system_prompt.as_deref(),
            Some(expected_system_prompt.as_str())
        );
        assert_eq!(
            requests[0].reasoning_effort,
            Some(ConcreteReasoningEffort::High)
        );
    }

    #[test]
    fn session_request_falls_back_to_default_allowed_tools_when_the_step_names_none() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-1")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("implement")]);

        manager
            .start_run(&workflow, start_input("ship it"))
            .unwrap();

        let requests = requests.lock().unwrap();
        let expected: Vec<String> = types::DEFAULT_ALLOWED_TOOLS
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect();
        assert_eq!(requests[0].allowed_tools, expected);
        assert!(requests[0].bash_allowlist.is_empty());
        assert_eq!(
            requests[0].system_prompt.as_deref(),
            Some(session::TASK_CONTROL_INSTRUCTIONS)
        );
        assert_eq!(requests[0].reasoning_effort, None);
    }

    #[test]
    fn runtime_applies_and_hides_turn_markers_before_publishing_text() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![SessionOutcome::Completed(SessionReport {
            turn_text: "Implemented it.\nDUCK:PR=500\nDUCK:TITLE=Improve runtime".to_owned(),
            ..SessionReport::default()
        })]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let run = manager
            .start_run(&workflow, start_input("markers"))
            .unwrap();

        let saved = manager.get_run(&run.id).unwrap();
        assert_eq!(saved.pr_number, Some(500.0));
        assert_eq!(saved.title_summary.as_deref(), Some("500: improve runtime"));
        let text = manager
            .read_events(&run.id)
            .into_iter()
            .find(|event| event.event_type == "text")
            .and_then(|event| {
                event
                    .extra
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        assert_eq!(text.as_deref(), Some("Implemented it."));
    }

    /// A session that calls `on_event` several times mid-turn, the way a real backend's
    /// process-output loop would — proof that `turn()`'s sink parameter is a live channel and
    /// not just plumbing that happens to be unused by [`FakeSession`].
    struct StreamingSession {
        chunks: Vec<EventInput>,
        outcome: Option<SessionOutcome>,
    }

    impl AgentSession for StreamingSession {
        fn turn(
            &mut self,
            on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
        ) -> Result<SessionOutcome, String> {
            for chunk in self.chunks.drain(..) {
                on_event(chunk).map_err(|error| error.to_string())?;
            }
            self.outcome
                .take()
                .ok_or_else(|| "streaming session has no outcome".to_owned())
        }
    }

    struct StreamingFactory(Mutex<Option<StreamingSession>>);

    impl SessionFactory for StreamingFactory {
        fn open(&self, _request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String> {
            self.0
                .lock()
                .unwrap()
                .take()
                .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                .ok_or_else(|| "streaming factory already opened its one session".to_owned())
        }
    }

    #[test]
    fn a_session_that_streams_several_events_mid_turn_persists_each_one_live() {
        let dir = tempdir().unwrap();
        let factory = StreamingFactory(Mutex::new(Some(StreamingSession {
            chunks: vec![
                EventInput::new("text").field("text", "Looking at the code…"),
                EventInput::new("tool-call")
                    .field("id", "call-1")
                    .field("tool", "Read")
                    .field("input", json!({"path": "src/lib.rs"})),
                EventInput::new("tool-result")
                    .field("toolCallId", "call-1")
                    .field("result", "ok")
                    .field("isError", false),
                EventInput::new("text").field("text", "Done. DUCK:DONE"),
            ],
            outcome: Some(SessionOutcome::Completed(SessionReport {
                turn_text: "Looking at the code…\nDone. DUCK:DONE".to_owned(),
                ..SessionReport::default()
            })),
        })));
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let run = manager
            .start_run(&workflow, start_input("stream me"))
            .unwrap();

        let events = manager.read_events(&run.id);
        let text_events: Vec<&str> = events
            .iter()
            .filter(|event| event.event_type == "text")
            .filter_map(|event| event.extra.get("text").and_then(Value::as_str))
            .collect();
        // Two live text chunks, each already marker-stripped by the sink — not one aggregated
        // blob appended after the turn finished, and the trailing `DUCK:DONE` never appears.
        assert_eq!(text_events, ["Looking at the code…", "Done."]);
        let tool_call = events
            .iter()
            .find(|event| event.event_type == "tool-call")
            .expect("tool-call event persisted live");
        assert_eq!(
            tool_call.extra.get("tool").and_then(Value::as_str),
            Some("Read")
        );
        let tool_result = events
            .iter()
            .find(|event| event.event_type == "tool-result")
            .expect("tool-result event persisted live");
        assert_eq!(
            tool_result.extra.get("toolCallId").and_then(Value::as_str),
            Some("call-1")
        );
        // Every streamed event carries the seq order it arrived in, proving these are discrete
        // live appends rather than one write reconstructed after the fact.
        let seqs: Vec<f64> = events.iter().map(|event| event.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(seqs, sorted);
        assert!(seqs.len() >= 4);
    }

    #[test]
    fn runtime_uses_injected_workspace_and_repository_lease_seams() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("leased")]);
        let acquired_workspace = Arc::new(Mutex::new(Vec::new()));
        let released_workspace = Arc::new(Mutex::new(Vec::new()));
        let acquired_repository = Arc::new(Mutex::new(Vec::new()));
        let released_repository = Arc::new(Mutex::new(Vec::new()));
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_project_id("project-a");
        manager.set_workspace_semaphore(RecordingWorkspaceSemaphore {
            acquired: acquired_workspace.clone(),
            released: released_workspace.clone(),
        });
        manager.set_repository_lease(RecordingRepositoryLease {
            acquired: acquired_repository.clone(),
            released: released_repository.clone(),
        });
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let run = manager
            .start_run(&workflow, start_input("leased run"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(acquired_workspace.lock().unwrap()[0].1, "project-a");
        assert_eq!(released_workspace.lock().unwrap().len(), 1);
        assert_eq!(&*acquired_repository.lock().unwrap(), &vec![run.id.clone()]);
        assert_eq!(&*released_repository.lock().unwrap(), &vec![run.id]);
    }

    #[test]
    fn runtime_retries_a_failed_check_only_within_its_bound() {
        let dir = tempdir().unwrap();
        let (factory, _requests) =
            fake_factory(vec![completed_session("first"), completed_session("retry")]);
        let checks = Arc::new(Mutex::new(std::collections::VecDeque::from([
            CheckResult {
                success: false,
                exit_code: 7,
                output: "bad".to_owned(),
            },
            CheckResult {
                success: true,
                exit_code: 0,
                output: "fixed".to_owned(),
            },
        ])));
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_check_executor(FakeChecks { results: checks });
        let workflow = workflow_with_steps(vec![
            agent_workflow_step("implement"),
            check_workflow_step("verify", Some("implement"), 1),
        ]);

        let run = manager
            .start_run(&workflow, start_input("retry me"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.steps[0].iterations, 2.0);
        assert_eq!(run.steps[0].status, StepStatus::Done);
        assert_eq!(run.steps[1].status, StepStatus::Done);
    }

    #[test]
    fn initial_prompt_images_are_persisted_and_reach_the_session_request() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("image-session")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let image = PromptImage {
            media_type: "image/png".to_owned(),
            data: "AQID".to_owned(),
        };
        let mut input = start_input("inspect this");
        input.images.push(image.clone());

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(run.task_images, Some(vec![image.data_url()]));
        assert_eq!(requests.lock().unwrap()[0].images, vec![image]);
    }

    #[test]
    fn runtime_fifo_blocks_the_second_job_and_finish_cleans_the_first() {
        let dir = tempdir().unwrap();
        let (factory, _requests) =
            fake_factory(vec![running_session(), completed_session("second")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_runtime_options(RuntimeOptions {
            max_parallel: 1,
            review_gate: false,
            ..RuntimeOptions::default()
        });
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let first = manager.start_run(&workflow, start_input("first")).unwrap();
        assert_eq!(first.status, RunStatus::Running);
        let second = manager.start_run(&workflow, start_input("second")).unwrap();
        assert_eq!(second.status, RunStatus::Queued);
        assert_eq!(
            manager.queue.queued().collect::<Vec<_>>(),
            [second.id.as_str()]
        );
        assert!(manager.cancel(&second.id).unwrap());
        assert_eq!(
            manager.get_run(&second.id).unwrap().status,
            RunStatus::Cancelled
        );
        assert!(manager.finish(&first.id).unwrap());
        assert_eq!(manager.get_run(&first.id).unwrap().status, RunStatus::Done);
        assert!(manager.active.is_empty());
        assert!(manager.jobs.is_empty());
    }

    #[test]
    fn runtime_delivers_followup_from_waiting_to_running_then_finish() {
        let dir = tempdir().unwrap();
        let (factory, _requests) =
            fake_factory_with_followups(vec![waiting_session(None)], vec![running_session()]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("park first"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Waiting);
        assert_eq!(
            manager.get_run(&run.id).unwrap().steps[0].status,
            StepStatus::Waiting
        );

        assert!(manager.send_message(&run.id, "carry on").unwrap());
        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Running);
        assert!(
            manager
                .active
                .get(&run.id)
                .is_some_and(|active| active.holds_slot)
        );
        assert!(
            manager
                .read_events(&run.id)
                .iter()
                .any(|event| event.event_type == "user-message")
        );

        assert!(manager.finish(&run.id).unwrap());
        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Done);
        assert!(!manager.is_active(&run.id));
    }

    #[test]
    fn autonomous_waiting_turns_are_nudged_until_the_session_completes() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory_with_followups(
            vec![waiting_session(None)],
            vec![completed_session("autonomous-complete")],
        );
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut input = start_input("finish without asking");
        input.autonomous = Some(true);

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.steps[0].status, StepStatus::Done);
        assert!(manager.read_events(&run.id).iter().any(|event| {
            event.event_type == "note"
                && event
                    .extra
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.starts_with("autonomous pass"))
        }));
    }

    #[test]
    fn autonomous_nudge_repairs_the_marker_without_expanding_the_task() {
        assert!(AUTONOMOUS_NUDGE.contains("may already have completed"));
        assert!(AUTONOMOUS_NUDGE.contains("Do not begin new work"));
        assert!(AUTONOMOUS_NUDGE.contains("search for unrelated work"));
        assert!(AUTONOMOUS_NUDGE.contains("reply with exactly DUCK:DONE"));
    }

    #[test]
    fn git_auto_with_changes_falls_back_to_review_without_a_production_dispatcher() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("git-auto-session")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_diff_inspector(FakeDiff(true));
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut input = start_input("commit this");
        input.git_auto = Some(true);

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(run.status, RunStatus::Review);
        assert!(manager.read_events(&run.id).iter().any(|event| {
            event.event_type == "note"
                && event
                    .extra
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("automatic commit/push failed"))
        }));
    }

    #[test]
    fn automatic_commit_subject_is_one_safe_line() {
        assert_eq!(
            commit_subject("  Add automatic commits  \nignored body").unwrap(),
            "Add automatic commits"
        );
        assert!(commit_subject("").is_err());
        assert!(commit_subject(&"x".repeat(73)).is_err());
        assert!(commit_subject("bad\u{0000} subject").is_err());
    }

    #[test]
    fn successful_automatic_git_action_finishes_without_the_review_gate() {
        let dir = tempdir().unwrap();
        let mut manager = RunManager::open(dir.path());
        let run = manager.create_run(create_input()).unwrap();
        manager.finish_git_auto(&run.id, Ok(())).unwrap();

        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Done);
        assert!(manager.read_events(&run.id).iter().any(|event| {
            event.event_type == "lifecycle"
                && event.extra.get("message").and_then(Value::as_str)
                    == Some("automatic commit and push finished")
        }));
    }

    #[test]
    fn auto_retries_the_original_prompt_on_the_next_provider_after_a_usage_limit() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![
            failed_session("You've hit your weekly limit · resets tomorrow"),
            completed_session("codex-success"),
        ]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut input = start_input("route this successfully");
        input.runner = Some(RunnerSelection::Auto);
        input.resolved_runner = Some(Runner::Claude);
        input.auto_runner_candidates = vec![Runner::Claude, Runner::Codex];
        input.autonomous = Some(true);

        let run = manager.start_run(&workflow, input).unwrap();

        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.runner, Some(Runner::Codex));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].runner, RunnerSelection::Claude);
        assert_eq!(requests[1].runner, RunnerSelection::Codex);
        assert_eq!(requests[0].prompt, requests[1].prompt);
        let notes: Vec<_> = manager
            .read_events(&run.id)
            .into_iter()
            .filter(|event| event.event_type == "note")
            .filter_map(|event| event.extra.get("message").cloned())
            .collect();
        assert!(notes.iter().any(|message| {
            message.as_str().is_some_and(|message| {
                message == "Auto routing · trying Claude · model provider default"
            })
        }));
        assert!(notes.iter().any(|message| {
            message.as_str().is_some_and(|message| {
                message == "Auto routing · Claude hit a usage limit — trying Codex"
            })
        }));
        assert!(!notes.iter().any(|message| {
            message
                .as_str()
                .is_some_and(|message| message.starts_with("autonomous pass"))
        }));
    }

    #[test]
    fn runtime_monitoring_followup_can_cancel_the_session() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory_with_followups(
            vec![waiting_session(Some(TurnMarkerDecision::Monitoring))],
            vec![cancelled_session()],
        );
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_runtime_options(RuntimeOptions {
            monitoring_wake_interval_minutes: Some(5),
            ..RuntimeOptions::default()
        });
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("monitor"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.activity, Some(RunActivity::Monitoring));
        assert!(run.monitoring_wake_at.is_some());

        assert!(
            manager
                .deliver_message(&run.id, "stop now", Vec::new())
                .unwrap()
        );
        let cancelled = manager.get_run(&run.id).unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        assert!(cancelled.activity.is_none());
        assert!(!manager.is_active(&run.id));
    }

    #[test]
    fn runtime_caps_monitoring_sessions_and_parks_additional_sessions() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![
            waiting_session(Some(TurnMarkerDecision::Monitoring)),
            waiting_session(Some(TurnMarkerDecision::Monitoring)),
        ]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_runtime_options(RuntimeOptions {
            max_monitoring_sessions: 1,
            ..RuntimeOptions::default()
        });
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let first = manager
            .start_run(&workflow, start_input("first monitor"))
            .unwrap();
        assert_eq!(first.status, RunStatus::Running);
        assert_eq!(first.activity, Some(RunActivity::Monitoring));

        let second = manager
            .start_run(&workflow, start_input("second monitor"))
            .unwrap();
        assert_eq!(second.status, RunStatus::Waiting);
        assert!(second.activity.is_none());
    }

    #[test]
    fn runtime_settles_changed_runs_at_review_and_finish_accepts_them() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("review-session")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_runtime_options(RuntimeOptions {
            max_parallel: 1,
            review_gate: true,
            ..RuntimeOptions::default()
        });
        manager.set_diff_inspector(FakeDiff(true));
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("review me"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Review);
        assert!(manager.finish(&run.id).unwrap());
        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Done);
    }

    #[test]
    fn runtime_starts_three_variants_with_one_group_and_fixed_hints() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![
            completed_session("a"),
            completed_session("b"),
            completed_session("c"),
        ]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let runs = manager
            .start_variants(&workflow, start_input("compare"), 3)
            .unwrap();
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|run| run.status == RunStatus::Done));
        assert_eq!(
            runs.iter()
                .map(|run| run.group_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1
        );
        assert_eq!(
            runs.iter()
                .map(|run| run.variant.as_deref())
                .collect::<Vec<_>>(),
            [Some("A"), Some("B"), Some("C")]
        );
        assert_eq!(requests.lock().unwrap().len(), 3);
        assert!(
            requests.lock().unwrap()[1]
                .prompt
                .contains("minimal, surgical")
        );
        assert!(
            requests.lock().unwrap()[2]
                .prompt
                .contains("thorough, structural")
        );
    }

    #[test]
    fn recover_rebuilds_queued_jobs_from_the_durable_workflow_definition() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("recovered")]);
        let mut first = RunManager::with_session_factory(dir.path(), factory);
        first.set_runtime_options(RuntimeOptions {
            max_parallel: 0,
            review_gate: false,
            ..RuntimeOptions::default()
        });
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let queued = first
            .start_run(&workflow, start_input("recover queued"))
            .unwrap();
        assert_eq!(queued.status, RunStatus::Queued);
        drop(first);

        let (factory, _requests) = fake_factory(vec![completed_session("recovered")]);
        let mut recovered = RunManager::with_session_factory(dir.path(), factory);
        let report = recovered.recover().unwrap();
        assert_eq!(report.queued, vec![queued.id.clone()]);
        assert_eq!(
            recovered.get_run(&queued.id).unwrap().status,
            RunStatus::Done
        );
        assert!(!recovered.is_active(&queued.id));
    }

    #[test]
    fn recover_settles_a_durable_waiting_session_without_resuming_it() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let run = first
            .create_workflow_run(&workflow, "recover waiting")
            .unwrap();
        first
            .update_step(
                &run.id,
                "work",
                StepPatch::new()
                    .set("status", StepStatus::Waiting)
                    .set("iterations", 1.0)
                    .set("sessionId", "waiting-session")
                    .set("backend", Runner::Claude),
            )
            .unwrap();
        first
            .update_run(&run.id, RunPatch::new().set("status", RunStatus::Waiting))
            .unwrap();
        drop(first);

        let mut recovered = RunManager::open(dir.path());
        let report = recovered.recover().unwrap();
        assert_eq!(report.settled, vec![run.id.clone()]);
        assert_eq!(recovered.get_run(&run.id).unwrap().status, RunStatus::Done);
        assert_eq!(
            recovered.get_run(&run.id).unwrap().steps[0].status,
            StepStatus::Done
        );
        assert!(!recovered.is_active(&run.id));
    }

    #[test]
    fn cancel_settles_a_loaded_waiting_run_without_a_live_session() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let run = first
            .create_workflow_run(&workflow, "cancel waiting")
            .unwrap();
        first
            .update_step(
                &run.id,
                "work",
                StepPatch::new()
                    .set("status", StepStatus::Waiting)
                    .set("iterations", 1.0)
                    .set("sessionId", "waiting-session")
                    .set("backend", Runner::Claude),
            )
            .unwrap();
        first
            .update_run(&run.id, RunPatch::new().set("status", RunStatus::Waiting))
            .unwrap();
        drop(first);

        let mut reopened = RunManager::open(dir.path());
        assert!(!reopened.is_active(&run.id));
        assert!(reopened.cancel(&run.id).unwrap());
        let settled = reopened.get_run(&run.id).unwrap();
        assert_eq!(settled.status, RunStatus::Cancelled);
        assert_eq!(settled.steps[0].status, StepStatus::Cancelled);
        assert!(settled.finished_at.is_some());
        assert!(!reopened.is_active(&run.id));
    }

    #[test]
    fn cancel_settles_a_loaded_queued_run_without_a_live_session() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let run = first
            .create_workflow_run(&workflow, "cancel queued")
            .unwrap();
        first
            .update_run(&run.id, RunPatch::new().set("status", RunStatus::Queued))
            .unwrap();
        drop(first);

        let mut reopened = RunManager::open(dir.path());
        assert!(!reopened.is_active(&run.id));
        assert!(reopened.cancel(&run.id).unwrap());
        let settled = reopened.get_run(&run.id).unwrap();
        assert_eq!(settled.status, RunStatus::Cancelled);
        assert!(settled.finished_at.is_some());
        assert!(!reopened.is_active(&run.id));
    }

    #[test]
    fn finish_settles_a_loaded_waiting_run_without_a_live_session() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let run = first
            .create_workflow_run(&workflow, "finish waiting")
            .unwrap();
        first
            .update_step(
                &run.id,
                "work",
                StepPatch::new()
                    .set("status", StepStatus::Waiting)
                    .set("iterations", 1.0)
                    .set("sessionId", "waiting-session")
                    .set("backend", Runner::Claude),
            )
            .unwrap();
        first
            .update_run(&run.id, RunPatch::new().set("status", RunStatus::Waiting))
            .unwrap();
        drop(first);

        let mut reopened = RunManager::open(dir.path());
        assert!(!reopened.is_active(&run.id));
        assert!(reopened.finish(&run.id).unwrap());
        let settled = reopened.get_run(&run.id).unwrap();
        assert_eq!(settled.status, RunStatus::Done);
        assert_eq!(settled.steps[0].status, StepStatus::Done);
        assert!(settled.finished_at.is_some());
        assert!(!reopened.is_active(&run.id));
    }

    #[test]
    fn recover_marks_a_running_record_interrupted_and_requeues_a_continuation() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let run = first
            .create_workflow_run(&workflow, "recover running")
            .unwrap();
        first
            .update_step(
                &run.id,
                "work",
                StepPatch::new()
                    .set("status", StepStatus::Running)
                    .set("iterations", 1.0)
                    .set("sessionId", "running-session")
                    .set("backend", Runner::Claude),
            )
            .unwrap();
        first
            .update_run(&run.id, RunPatch::new().set("status", RunStatus::Running))
            .unwrap();
        drop(first);

        let (factory, requests) = fake_factory(vec![completed_session("continued")]);
        let mut recovered = RunManager::with_session_factory(dir.path(), factory);
        let report = recovered.recover().unwrap();
        assert_eq!(report.resumed, vec![run.id.clone()]);
        assert_eq!(recovered.get_run(&run.id).unwrap().status, RunStatus::Done);
        assert_eq!(
            requests.lock().unwrap()[0].session_id.as_deref(),
            Some("running-session")
        );
        assert!(
            recovered
                .get_run(&run.id)
                .unwrap()
                .steps
                .iter()
                .any(|step| step.id == "continue-1" && step.status == StepStatus::Done)
        );
    }

    #[test]
    fn quota_reconciliation_uses_deadlines_and_account_holds_without_timers() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut first = RunManager::open(dir.path());
        let future = first
            .create_workflow_run(&workflow, "future limit")
            .unwrap();
        first
            .update_step(
                &future.id,
                "work",
                StepPatch::new().set("sessionId", "future-session"),
            )
            .unwrap();
        first
            .update_run(
                &future.id,
                RunPatch::new()
                    .set("status", RunStatus::Failed)
                    .set("autoResumeAt", "2099-01-01T00:00:00.000Z"),
            )
            .unwrap();
        let queued = first
            .create_workflow_run(&workflow, "blocked fresh work")
            .unwrap();
        first
            .update_run(
                &queued.id,
                RunPatch::new()
                    .set("runner", Runner::Claude)
                    .set("agentProfile", "default"),
            )
            .unwrap();
        let plan = first.reconcile_quota_at("2026-01-01T00:00:00.000Z");
        assert_eq!(plan.scheduled, vec![future.id.clone()]);
        assert_eq!(plan.blocked_queue, vec![queued.id.clone()]);
        assert!(plan.holds.deadline.contains("claude:default"));

        let due = first.create_workflow_run(&workflow, "due limit").unwrap();
        first
            .update_step(
                &due.id,
                "work",
                StepPatch::new().set("sessionId", "due-session"),
            )
            .unwrap();
        first
            .update_run(
                &due.id,
                RunPatch::new()
                    .set("status", RunStatus::Failed)
                    .set("autoResumeAt", "2020-01-01T00:00:00.000Z")
                    .set("agentProfile", "second"),
            )
            .unwrap();
        first.set_runtime_options(RuntimeOptions {
            max_parallel: 0,
            review_gate: false,
            ..RuntimeOptions::default()
        });
        let report = first
            .reconcile_auto_resumes("2026-01-01T00:00:00.000Z")
            .unwrap();
        assert_eq!(report.requeued, vec![due.id.clone()]);
        assert_eq!(first.get_run(&due.id).unwrap().status, RunStatus::Queued);
        assert_eq!(
            first.get_run(&due.id).unwrap().auto_resume_attempts,
            Some(1.0)
        );
        assert!(
            first
                .reconcile_quota_at("2026-01-01T00:00:00.000Z")
                .holds
                .in_flight
                .contains("claude:second")
        );
    }

    #[test]
    fn disabled_auto_resume_keeps_due_usage_limited_runs_parked() {
        let dir = tempdir().unwrap();
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut manager = RunManager::open(dir.path());
        let run = manager
            .create_workflow_run(&workflow, "wait for quota")
            .unwrap();
        manager
            .update_step(
                &run.id,
                "work",
                StepPatch::new().set("sessionId", "quota-session"),
            )
            .unwrap();
        manager
            .update_run(
                &run.id,
                RunPatch::new()
                    .set("status", RunStatus::Failed)
                    .set("autoResumeAt", "2020-01-01T00:00:00.000Z"),
            )
            .unwrap();
        manager.set_runtime_options(RuntimeOptions {
            auto_resume_on_usage_limit: false,
            ..RuntimeOptions::default()
        });

        let report = manager
            .reconcile_auto_resumes("2026-01-01T00:00:00.000Z")
            .unwrap();

        assert_eq!(report.plan.due, vec![run.id.clone()]);
        assert!(report.requeued.is_empty());
        assert_eq!(manager.get_run(&run.id).unwrap().status, RunStatus::Failed);
        assert_eq!(
            manager.get_run(&run.id).unwrap().auto_resume_at.as_deref(),
            Some("2020-01-01T00:00:00.000Z")
        );
    }

    #[test]
    fn continuation_persists_runner_and_model_override_and_starts_fresh_for_a_switch() {
        let dir = tempdir().unwrap();
        let (factory, requests) =
            fake_factory(vec![completed_session("old"), completed_session("new")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let mut input = start_input("continue me");
        input.model = Some("sonnet".to_owned());
        let run = manager.start_run(&workflow, input).unwrap();
        let result = manager
            .continue_run(
                &run.id,
                ContinueOptions {
                    text: Some("keep going".to_owned()),
                    images: Vec::new(),
                    runner: Some(RunnerSelection::Codex),
                    model: Some("gpt-5.1-codex".to_owned()),
                },
            )
            .unwrap();
        assert!(result.ok);
        manager.run_to_completion().unwrap();
        let continued = manager.get_run(&run.id).unwrap();
        assert_eq!(continued.runner, Some(Runner::Codex));
        assert_eq!(continued.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(requests.lock().unwrap()[1].runner, RunnerSelection::Codex);
        assert_eq!(requests.lock().unwrap()[1].session_id, None);
        assert_eq!(requests.lock().unwrap()[1].prompt, "keep going");
        assert!(
            continued
                .steps
                .iter()
                .any(|step| step.id == "continue-1" && step.status == StepStatus::Done)
        );
        assert!(
            continued
                .workflow_def
                .as_ref()
                .is_some_and(|definition| definition
                    .steps
                    .iter()
                    .any(|step| step.id == "continue-1"))
        );
    }

    #[test]
    fn continuation_across_a_runner_switch_announces_the_dropped_session() {
        let dir = tempdir().unwrap();
        let (factory, _requests) =
            fake_factory(vec![completed_session("old"), completed_session("new")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("continue me"))
            .unwrap();
        manager
            .continue_run(
                &run.id,
                ContinueOptions {
                    text: Some("keep going".to_owned()),
                    runner: Some(RunnerSelection::Codex),
                    ..ContinueOptions::default()
                },
            )
            .unwrap();
        assert!(manager.read_events(&run.id).iter().any(|event| {
            event.event_type == "note"
                && event
                    .extra
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| {
                        message.contains("switching from claude to codex")
                            && message.contains("not resumed")
                    })
        }));
    }

    #[test]
    fn continuation_keeps_the_session_when_the_runner_stays_the_same() {
        let dir = tempdir().unwrap();
        let (factory, requests) =
            fake_factory(vec![completed_session("old"), completed_session("resumed")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("resume me"))
            .unwrap();
        manager
            .continue_run(
                &run.id,
                ContinueOptions {
                    runner: Some(RunnerSelection::Claude),
                    text: Some("resume".to_owned()),
                    ..ContinueOptions::default()
                },
            )
            .unwrap();
        manager.run_to_completion().unwrap();
        assert_eq!(requests.lock().unwrap()[1].runner, RunnerSelection::Claude);
        assert_eq!(
            requests.lock().unwrap()[1].session_id.as_deref(),
            Some("old")
        );
        assert!(
            manager.get_run(&run.id).unwrap().steps.iter().any(
                |step| step.id == "continue-1" && step.session_id.as_deref() == Some("resumed")
            )
        );
    }

    #[test]
    fn continuation_persists_the_follow_up_before_starting_the_agent() {
        let dir = tempdir().unwrap();
        let image = PromptImage {
            media_type: "image/png".to_owned(),
            data: "AQID".to_owned(),
        };
        let (factory, _requests) =
            fake_factory(vec![completed_session("old"), completed_session("resumed")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("first prompt"))
            .unwrap();

        let result = manager
            .continue_run(
                &run.id,
                ContinueOptions {
                    text: Some("second prompt".to_owned()),
                    images: vec![image.clone()],
                    ..ContinueOptions::default()
                },
            )
            .unwrap();

        assert!(result.ok);
        let events = manager.read_events(&run.id);
        let follow_up = events
            .iter()
            .find(|event| event.event_type == "user-message")
            .unwrap();
        assert_eq!(follow_up.step_id.as_deref(), Some("continue-1"));
        assert_eq!(
            follow_up.extra.get("text").and_then(Value::as_str),
            Some("second prompt")
        );
        assert_eq!(
            follow_up.extra.get("imageCount").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            follow_up.extra.get("images"),
            Some(&json!([image.data_url()]))
        );
        let follow_up_index = events
            .iter()
            .position(|event| event.event_type == "user-message")
            .unwrap();
        let continuation_start_index = events
            .iter()
            .position(|event| {
                event.event_type == "step-start" && event.step_id.as_deref() == Some("continue-1")
            })
            .unwrap();
        assert!(follow_up_index < continuation_start_index);
    }
}
