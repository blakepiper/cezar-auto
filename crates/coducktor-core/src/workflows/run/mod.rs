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
#[path = "manager/dispatch.rs"]
mod manager_dispatch;
#[path = "manager/lifecycle.rs"]
mod manager_lifecycle;
#[path = "manager/persistence.rs"]
mod manager_persistence;
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
    MarkerRefs, QueuedMessage, RunActivity, RunRecord, RunStatus, StepKind, StepState, StepStatus,
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
    pub fn step_id(&self) -> io::Result<&str> {
        workflow_step(&self.workflow, self.step_index).map(|step| step.id.as_str())
    }
}

fn workflow_step(
    workflow: &WorkflowDef,
    step_index: usize,
) -> io::Result<&coducktor_contract::workflows::WorkflowStepDef> {
    workflow.steps.get(step_index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workflow runtime referenced missing step {step_index} of {}",
                workflow.steps.len()
            ),
        )
    })
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
    /// A user follow-up arrived while the provider turn was in flight. The manager has removed
    /// it from the durable queue and appended its user-message event; the caller must deliver it
    /// through this same live session before reporting the result through `apply_message_turn`.
    QueuedMessage {
        active: Box<RuntimeActive>,
        prompt: String,
        images: Vec<PromptImage>,
    },
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
        let mut loaded = load.records().to_vec();
        // `waiting` used to mean any parked session. On upgrade, retain attention only when the
        // durable transcript proves that a structured ask is still unanswered; every other
        // legacy record becomes the new neutral parked state.
        for run in &mut loaded {
            if run.status == RunStatus::Waiting
                && !events::has_pending_ask(&events::events_path(&data_dir, &run.id))
            {
                run.status = RunStatus::Idle;
            }
        }
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
        RunStatus::Idle => "idle",
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
            RunStatus::Running | RunStatus::Idle | RunStatus::Waiting | RunStatus::Queued
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
    let step = run.steps.get(step_index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "run step disappeared before patch",
        )
    })?;
    let value = serde_json::to_value(step).map_err(|error| {
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
    let patched = serde_json::from_value(Value::Object(object)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid step patch: {error}"),
        )
    })?;
    let step = run.steps.get_mut(step_index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "run step disappeared during patch",
        )
    })?;
    *step = patched;
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

fn new_queued_message_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("message-{nanos:x}-{counter:x}")
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
        assert_eq!(
            decide_turn_marker("ordinary final answer", true, false),
            TurnMarkerDecision::Idle
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

    #[test]
    fn missing_runtime_step_is_reported_instead_of_indexing() {
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let error = workflow_step(&workflow, 4).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("missing step 4 of 1"));
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
    fn runtime_delivers_followup_from_idle_to_running_then_finish() {
        let dir = tempdir().unwrap();
        let (factory, _requests) =
            fake_factory_with_followups(vec![waiting_session(None)], vec![running_session()]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("park first"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Idle);
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
    fn structured_ask_parks_in_needs_input_while_a_plain_turn_parks_idle() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![
            waiting_session(None),
            waiting_session(Some(TurnMarkerDecision::Ask)),
        ]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);

        let plain = manager
            .start_run(&workflow, start_input("plain response"))
            .unwrap();
        let ask = manager
            .start_run(&workflow, start_input("structured question"))
            .unwrap();

        assert_eq!(plain.status, RunStatus::Idle);
        assert_eq!(ask.status, RunStatus::Waiting);
    }

    #[test]
    fn legacy_waiting_records_upgrade_to_idle_unless_the_log_has_a_pending_ask() {
        let dir = tempdir().unwrap();
        let (plain_id, ask_id) = {
            let mut manager = RunManager::new(dir.path());
            let plain = manager.create_run(create_input()).unwrap();
            let mut ask_input = create_input();
            ask_input.title = "question".to_owned();
            let ask = manager.create_run(ask_input).unwrap();
            manager
                .update_run(&plain.id, RunPatch::new().set("status", RunStatus::Waiting))
                .unwrap();
            manager
                .append_event(
                    &ask.id,
                    EventInput::new("ask.requested")
                        .field("requestId", "question-1")
                        .field(
                            "questions",
                            vec![serde_json::json!({"question": "Choose?"})],
                        ),
                )
                .unwrap();
            manager
                .update_run(&ask.id, RunPatch::new().set("status", RunStatus::Waiting))
                .unwrap();
            (plain.id, ask.id)
        };

        let reopened = RunManager::open(dir.path());
        assert_eq!(reopened.get_run(&plain_id).unwrap().status, RunStatus::Idle);
        assert_eq!(
            reopened.get_run(&ask_id).unwrap().status,
            RunStatus::Waiting
        );
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
        assert_eq!(second.status, RunStatus::Idle);
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
