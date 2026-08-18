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

use coducktor_contract::events::RunEvent;
use coducktor_contract::runs::{
    MarkerRefs, RunActivity, RunRecord, RunStatus, StepKind, StepState, StepStatus,
};
use coducktor_contract::workflows::WorkflowDef;
use coducktor_contract::{ConcreteReasoningEffort, ReasoningEffort, Runner, RunnerSelection};
use serde::Serialize;
use serde_json::{Map, Value};

use super::types;
use crate::runs::events;
use crate::runs::store;
use crate::runs::task_markers::{self, TaskMarkers};
use crate::time::{is_zod_datetime, now_iso8601};

const AUTONOMOUS_NUDGE: &str = "Continue working autonomously until the task is fully complete. Do not ask me for confirmation or clarification — make reasonable assumptions and proceed. When everything is done and verified, end your final response with a line containing exactly DUCK:DONE.";

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
    pub worktree: Option<bool>,
    pub group_id: Option<String>,
    pub variant: Option<String>,
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

/// Count occupied slots: ordinary waiting runs do not hold a slot, and only the configured number
/// of monitoring sessions receive that same exemption.
pub fn busy_slots(
    active: usize,
    starting: usize,
    waiting: usize,
    monitoring: usize,
    max_monitoring_sessions: usize,
) -> usize {
    let ordinary_waiting = waiting.saturating_sub(monitoring);
    let exempt_monitoring = monitoring.min(max_monitoring_sessions);
    active
        .saturating_add(starting)
        .saturating_sub(ordinary_waiting)
        .saturating_sub(exempt_monitoring)
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
    pub agent_profile: Option<String>,
    pub system_prompt: Option<String>,
    pub autonomous: Option<bool>,
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
    /// The directory the agent should run in. The current manager passes the configured
    /// repository root; worktree selection is handled by higher-level orchestration.
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
pub trait SessionFactory: Send {
    fn open(&mut self, request: SessionRequest) -> Result<Box<dyn AgentSession + Send>, String>;

    fn request_cancel(&mut self, _run_id: &str) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub success: bool,
    pub output: String,
}

/// Check execution is injected for the same reason as sessions: core owns workflow semantics, not
/// a shell/process policy.
pub trait CheckExecutor: Send {
    fn run(&mut self, command: &str) -> Result<CheckResult, String>;
}

/// Review settlement asks an injected diff reader whether the run has changes. This keeps Git
/// worktree I/O out of the runtime foundation while preserving the review decision.
pub trait DiffInspector: Send {
    fn has_diff(&mut self, run: &RunRecord) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub max_parallel: usize,
    pub review_gate: bool,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            max_parallel: 2,
            review_gate: false,
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

struct RuntimeActive {
    workflow: WorkflowDef,
    step_index: usize,
    next_index: usize,
    retry_counts: BTreeMap<String, u32>,
    session: Box<dyn AgentSession>,
    holds_slot: bool,
    plan_checkpoint: context_refresh::PlanCheckpoint,
    auto_continues: u32,
}

/// A stateful, synchronous facade over the durable run files.
pub struct RunManager {
    data_dir: PathBuf,
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
    project_id: String,
    workspace_semaphore: Option<Box<dyn WorkspaceSemaphore>>,
    repository_lease: Option<Box<dyn RepositoryRootLease>>,
    workspace_holds: BTreeSet<String>,
    repository_holds: BTreeSet<String>,
    plan_checkpoints: BTreeMap<String, context_refresh::PlanCheckpoint>,
    pending_context_prompts: BTreeMap<String, String>,
    intelligent_context_refresh: bool,
}

impl RunManager {
    /// Open a live manager. Active records are retained so callers can reconcile or resume them.
    pub fn open(data_dir: impl Into<PathBuf>) -> Self {
        Self::open_with_keep_live(data_dir, true)
    }

    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::open(data_dir)
    }

    pub fn for_repo(repo_root: &Path) -> Self {
        Self::open(repo_root.join(".ai").join("coducktor"))
    }

    pub fn with_session_factory(
        data_dir: impl Into<PathBuf>,
        session_factory: impl SessionFactory + 'static,
    ) -> Self {
        let mut manager = Self::open(data_dir);
        manager.session_factory = Some(Box::new(session_factory));
        manager
    }

    pub fn open_with_keep_live(data_dir: impl Into<PathBuf>, keep_live: bool) -> Self {
        let data_dir = data_dir.into();
        let _ = fs::create_dir_all(data_dir.join("runs"));
        let loaded = store::load_run_index(&store::index_path(&data_dir), keep_live);
        let runs = loaded
            .into_iter()
            .map(|run| (run.id.clone(), run))
            .collect();
        Self {
            data_dir,
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
            project_id: "default".to_owned(),
            workspace_semaphore: None,
            repository_lease: None,
            workspace_holds: BTreeSet::new(),
            repository_holds: BTreeSet::new(),
            plan_checkpoints: BTreeMap::new(),
            pending_context_prompts: BTreeMap::new(),
            intelligent_context_refresh: false,
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The repo root a session should run in — `data_dir` minus the `.ai/coducktor` suffix
    /// `for_repo` appended, so this only round-trips correctly for managers opened that way
    /// (every production caller). A manager opened directly against an arbitrary `data_dir`
    /// (most unit tests) gets a nonsensical answer back, but nothing in this crate reads it for
    /// anything other than a `SessionRequest`'s `cwd`, which those tests' fakes never inspect.
    fn repo_root(&self) -> PathBuf {
        self.data_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
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
        Ok(true)
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

    fn persist(&self) -> io::Result<()> {
        fs::create_dir_all(self.data_dir.join("runs"))?;
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        store::write_run_index(&store::index_path(&self.data_dir), &records)
    }

    /// Flush is kept explicit for callers that want a named shutdown boundary. Mutations are
    /// already written synchronously before they return.
    pub fn flush(&self) -> io::Result<()> {
        self.persist()
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
            worktree: input.worktree,
            group_id: input.group_id,
            variant: input.variant,
            status: RunStatus::Queued,
            created_at: created_at.clone(),
            updated_at: Some(created_at),
            tokens_used: 0.0,
            archived: false,
            steps: input.steps.into_iter().map(step_from_seed).collect(),
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
        create.worktree = input.worktree;
        create.task_images = (!input.images.is_empty())
            .then(|| input.images.iter().map(PromptImage::data_url).collect());
        let run = self.create_run(create)?;
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
        self.pump()?;
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
        self.pump()?;
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
        events::append_event(&path, &event)?;
        // Event append is meaningful activity. Keep read/unread and archive mutations on their
        // separate timestamps by stamping here instead of in the generic record replacement.
        let updated_run = if let Some(run) = self.runs.get_mut(run_id) {
            run.updated_at = Some(event.ts.clone());
            Some(run.clone())
        } else {
            None
        };
        if updated_run.is_some() {
            self.persist()?;
        }
        self.seqs.insert(run_id.to_owned(), seq);
        if let Some(run) = &updated_run {
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
        self.pump()?;
        Ok(report)
    }

    /// Drain queued jobs while an injected runtime slot is available. The method is synchronous on
    /// purpose: the engine can call it from its scheduler, while unit tests can observe every
    /// transition without sleeps or a process-wide executor.
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

    fn runtime_busy_slots(&self) -> usize {
        self.active
            .values()
            .filter(|active| active.holds_slot)
            .count()
            .saturating_add(self.queue.starting().count())
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
        if !self.repository_holds.contains(run_id) {
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
        let mut plan_checkpoint = self.plan_checkpoints.remove(run_id).unwrap_or_default();
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
        let mut initial_images_sent = false;

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
                let result = match self.check_executor.as_mut() {
                    Some(executor) => executor.run(command),
                    None => Err("check executor unavailable".to_owned()),
                };
                let result = match result {
                    Ok(result) => result,
                    Err(error) => CheckResult {
                        success: false,
                        output: error,
                    },
                };
                self.append_event(
                    run_id,
                    EventInput::new("check-output")
                        .step(step.id.clone())
                        .field("command", command)
                        .field("text", result.output.clone())
                        .field("exitCode", if result.success { 0 } else { 1 }),
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
            let request = SessionRequest {
                run_id: run_id.to_owned(),
                step_id: step.id.clone(),
                prompt,
                images: if continuation_step {
                    std::mem::take(&mut continuation_images)
                } else if initial_images_sent {
                    Vec::new()
                } else {
                    initial_images_sent = true;
                    self.get_run(run_id)
                        .map(hydrate_queued_images)
                        .unwrap_or_default()
                },
                runner,
                model,
                session_id,
                continuation: continuation_step,
                cwd: self.repo_root(),
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
            self.update_step(run_id, &step.id, step_affinity)?;
            let opened = match self.session_factory.as_mut() {
                Some(factory) => factory.open(request),
                None => Err("session factory unavailable".to_owned()),
            };
            let mut session = match opened {
                Ok(session) => session,
                Err(error) => {
                    self.fail_run(run_id, Some(&step.id), error)?;
                    return Ok(());
                }
            };
            let fallback_session_id = session.session_id();
            let turn_result = session.turn(&mut self.event_sink(run_id, &step.id));
            let mut outcome = match turn_result {
                Ok(_) if cancellation.is_requested() => {
                    SessionOutcome::Cancelled(SessionReport::default())
                }
                Ok(outcome) => outcome,
                Err(_) if cancellation.is_requested() => {
                    SessionOutcome::Cancelled(SessionReport::default())
                }
                Err(error) => {
                    self.fail_run(run_id, Some(&step.id), error)?;
                    return Ok(());
                }
            };
            let mut auto_continues = 0;
            loop {
                let report = session_outcome_report(&outcome).clone();
                self.apply_session_report(run_id, &step.id, &report, fallback_session_id.clone())?;
                self.apply_session_markers(run_id, &report.turn_text)?;
                let refresh_prompt = if self.intelligent_context_refresh {
                    report.plan_entries.as_deref().and_then(|entries| {
                        context_refresh::observe_plan(&mut plan_checkpoint, entries, true)
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
                        &step.id,
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
                        .insert(run_id.to_owned(), plan_checkpoint);
                    self.pending_context_prompts
                        .insert(run_id.to_owned(), refresh_prompt);
                    self.jobs.insert(
                        run_id.to_owned(),
                        RuntimeJob::Workflow {
                            workflow: workflow.clone(),
                            start_index: index,
                            retry_counts,
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
                    self.pump()?;
                    return Ok(());
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
                    && auto_continues < MAX_AUTONOMOUS_CONTINUES;
                if !should_nudge {
                    break;
                }
                auto_continues += 1;
                self.append_event(
                    run_id,
                    EventInput::new("note").field(
                        "message",
                        format!("autonomous pass {auto_continues} of {MAX_AUTONOMOUS_CONTINUES}"),
                    ),
                )?;
                outcome = match session.send_message(
                    AUTONOMOUS_NUDGE,
                    &[],
                    &mut self.event_sink(run_id, &step.id),
                ) {
                    Ok(_) if cancellation.is_requested() => {
                        SessionOutcome::Cancelled(SessionReport::default())
                    }
                    Ok(outcome) => outcome,
                    Err(_) if cancellation.is_requested() => {
                        SessionOutcome::Cancelled(SessionReport::default())
                    }
                    Err(error) => SessionOutcome::Failed {
                        message: error,
                        report: SessionReport::default(),
                    },
                };
            }

            match outcome {
                SessionOutcome::Completed(_) => {
                    self.complete_step(run_id, &step.id, None)?;
                    index += 1;
                    continue;
                }
                SessionOutcome::Waiting(report) => {
                    self.park_session(
                        run_id,
                        RuntimeActive {
                            workflow: workflow.clone(),
                            step_index: index,
                            next_index: index + 1,
                            retry_counts,
                            session,
                            holds_slot: false,
                            plan_checkpoint: plan_checkpoint.clone(),
                            auto_continues,
                        },
                        report.decision == Some(TurnMarkerDecision::Monitoring),
                    )?;
                    return Ok(());
                }
                SessionOutcome::Running(_) => {
                    self.park_session(
                        run_id,
                        RuntimeActive {
                            workflow: workflow.clone(),
                            step_index: index,
                            next_index: index + 1,
                            retry_counts,
                            session,
                            holds_slot: true,
                            plan_checkpoint: plan_checkpoint.clone(),
                            auto_continues,
                        },
                        false,
                    )?;
                    return Ok(());
                }
                SessionOutcome::Failed { message, .. } => {
                    self.fail_run(run_id, Some(&step.id), message)?;
                    return Ok(());
                }
                SessionOutcome::Cancelled(_) => {
                    self.cancel_run_after_session(run_id, &step.id)?;
                    return Ok(());
                }
            }
        }

        self.settle_success(run_id)
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
        active: RuntimeActive,
        monitoring: bool,
    ) -> io::Result<()> {
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
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", status)
                .set("activity", activity),
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

    fn handle_active_outcome(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        outcome: SessionOutcome,
    ) -> io::Result<()> {
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
            return self.pump();
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
            let next = match active.session.send_message(
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
            return self.handle_active_outcome(run_id, active, next);
        }
        match outcome {
            SessionOutcome::Failed { message, .. } => {
                self.fail_run(run_id, Some(&step_id), message)
            }
            SessionOutcome::Cancelled(_) => self.cancel_run_after_session(run_id, &step_id),
            SessionOutcome::Running(_) => {
                active.holds_slot = true;
                self.park_session(run_id, active, false)
            }
            SessionOutcome::Waiting(report) => {
                active.holds_slot = false;
                self.park_session(
                    run_id,
                    active,
                    report.decision == Some(TurnMarkerDecision::Monitoring),
                )
            }
            SessionOutcome::Completed(_) => {
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
                    self.settle_success(run_id)?;
                }
                self.pump()
            }
        }
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

    /// Finish a parked session, an accepted review, or a waiting record with no
    /// process-local runtime. A successful finish continues any remaining
    /// workflow steps, or performs the same review-gate settlement as a
    /// naturally completed workflow.
    pub fn finish(&mut self, run_id: &str) -> io::Result<bool> {
        let Some(mut active) = self.active.remove(run_id) else {
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
                return Ok(true);
            }
            if self
                .get_run(run_id)
                .is_some_and(|run| run.status == RunStatus::Waiting)
            {
                self.settle_steps(run_id, StepStatus::Done)?;
                self.settle_success(run_id)?;
                self.pump()?;
                return Ok(true);
            }
            return Ok(false);
        };
        let step_id = active.workflow.steps[active.step_index].id.clone();
        let finish_result = active
            .session
            .finish(&mut self.event_sink(run_id, &step_id));
        let outcome = match finish_result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.active.insert(run_id.to_owned(), active);
                return Err(io::Error::other(format!("finish failed: {error}")));
            }
        };
        if let Err(error) = self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", "session closed by user"),
        ) {
            self.active.insert(run_id.to_owned(), active);
            return Err(error);
        }
        let outcome = match outcome {
            SessionOutcome::Running(report) | SessionOutcome::Waiting(report) => {
                SessionOutcome::Completed(report)
            }
            other => other,
        };
        self.handle_active_outcome(run_id, active, outcome)?;
        Ok(true)
    }

    /// Deliver a user follow-up to a parked active session. The session outcome owns the next
    /// state transition: `Running` reacquires a slot, `Waiting`/monitoring releases it again, and
    /// terminal outcomes use the same cleanup path as an initial turn.
    pub fn send_message(&mut self, run_id: &str, prompt: impl Into<String>) -> io::Result<bool> {
        self.deliver_message(run_id, prompt, Vec::new())
    }

    /// Backend-neutral delivery seam. A durable `user-message` event is written before invoking
    /// the injected session, matching the transcript contract even when the session declines the
    /// delivery. No active session means the caller should use `continue_run` instead.
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
        let image_urls = images.iter().map(PromptImage::data_url).collect::<Vec<_>>();
        self.append_event(
            run_id,
            EventInput::new("user-message")
                .step(step_id.clone())
                .field("text", prompt.clone())
                .field("imageCount", image_urls.len())
                .field("images", image_urls),
        )?;
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
        self.handle_active_outcome(run_id, active, outcome)?;
        Ok(true)
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
        let Some(session_step) = run
            .steps
            .iter()
            .rev()
            .find(|step| step.session_id.is_some())
        else {
            return Ok(ContinueResult::error("no agent session to resume"));
        };
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
            .backend
            .or(run.runner)
            .unwrap_or(Runner::Claude);
        let resume_session = (target_concrete == Some(session_backend))
            .then(|| session_step.session_id.clone())
            .flatten();

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
        assert_eq!(busy_slots(4, 1, 2, 2, 1), 4);
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
        fn open(
            &mut self,
            request: SessionRequest,
        ) -> Result<Box<dyn AgentSession + Send>, String> {
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
        fn run(&mut self, _command: &str) -> Result<CheckResult, String> {
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
    fn runtime_executes_agent_and_check_steps_and_persists_session_usage() {
        let dir = tempdir().unwrap();
        let (factory, requests) = fake_factory(vec![completed_session("session-1")]);
        let checks = Arc::new(Mutex::new(std::collections::VecDeque::from([
            CheckResult {
                success: true,
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

        manager.pump().unwrap();
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
        // `for_repo` (not `with_session_factory`'s raw-`data_dir` shortcut) so `repo_root()`
        // round-trips back to `dir.path()`, matching the production path.
        let mut manager = RunManager::for_repo(dir.path());
        manager.set_session_factory(factory);
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

    struct StreamingFactory(Option<StreamingSession>);

    impl SessionFactory for StreamingFactory {
        fn open(
            &mut self,
            _request: SessionRequest,
        ) -> Result<Box<dyn AgentSession + Send>, String> {
            self.0
                .take()
                .map(|session| Box::new(session) as Box<dyn AgentSession + Send>)
                .ok_or_else(|| "streaming factory already opened its one session".to_owned())
        }
    }

    #[test]
    fn a_session_that_streams_several_events_mid_turn_persists_each_one_live() {
        let dir = tempdir().unwrap();
        let factory = StreamingFactory(Some(StreamingSession {
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
        }));
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
                output: "bad".to_owned(),
            },
            CheckResult {
                success: true,
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
    fn runtime_monitoring_followup_can_cancel_the_session() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory_with_followups(
            vec![waiting_session(Some(TurnMarkerDecision::Monitoring))],
            vec![cancelled_session()],
        );
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        let workflow = workflow_with_steps(vec![agent_workflow_step("work")]);
        let run = manager
            .start_run(&workflow, start_input("monitor"))
            .unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.activity, Some(RunActivity::Monitoring));

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
    fn runtime_settles_changed_runs_at_review_and_finish_accepts_them() {
        let dir = tempdir().unwrap();
        let (factory, _requests) = fake_factory(vec![completed_session("review-session")]);
        let mut manager = RunManager::with_session_factory(dir.path(), factory);
        manager.set_runtime_options(RuntimeOptions {
            max_parallel: 1,
            review_gate: true,
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
}
