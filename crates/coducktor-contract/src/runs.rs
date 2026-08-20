use serde::{Deserialize, Serialize};

use crate::github::ReferenceStatus;
use crate::health::{Runner, RunnerSelection};
use crate::reasoning::{ConcreteReasoningEffort, ReasoningEffort};
use crate::routing::{RoutingAttempt, RoutingDecision, RoutingIntent, RoutingWait};
use crate::workflows::{WorkflowDef, WorkflowStepDef};
use crate::workspace::QuotaProvider;

/// `RunStatus` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    #[default]
    Queued,
    Running,
    Waiting,
    Review,
    Done,
    Failed,
    Cancelled,
}

/// `RunActivity` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunActivity {
    Monitoring,
}

/// `ProviderQuotaBlockedReason` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuotaBlockedReason {
    #[serde(rename = "type")]
    pub reason_type: ProviderQuotaBlockedReasonType,
    pub providers: Vec<QuotaProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
}

/// The literal type discriminator in `ProviderQuotaBlockedReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderQuotaBlockedReasonType {
    #[serde(rename = "provider_quota")]
    ProviderQuota,
}

/// `StepStatus` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Waiting,
    Review,
    Done,
    Failed,
    Cancelled,
    Skipped,
}

/// `StepState` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepState {
    pub id: String,
    pub name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub iterations: f64,
    pub tokens_used: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_invocations_started: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_invocations_observed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_turns_started: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_turns_recorded: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_invocation_epoch: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<Runner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_generation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_intent: Option<RoutingIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_decision: Option<RoutingDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_wait: Option<RoutingWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_attempts: Option<Vec<RoutingAttempt>>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The agent/check discriminator in a step state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Agent,
    Check,
}

/// `DiffStat` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub adds: f64,
    pub dels: f64,
    pub files: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repointed: Option<bool>,
}

/// `QueuedMessage` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    pub created_at: String,
}

/// `ProcessUsage` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessUsage {
    pub cpu_pct: f64,
    pub rss_bytes: f64,
    pub proc_count: f64,
}

/// The legacy run provenance stamp retained after the automation subsystem is removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationProvenance {
    pub automation_id: String,
    pub automation_revision: f64,
    pub receipt_id: String,
    pub event: String,
    pub github_url: String,
}

/// `RunRecord` contract shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<DiffStat>,
    pub workflow: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_messages: Option<Vec<QueuedMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_images: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<Runner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
    /// `true` when this run commits and pushes automatically at natural checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_auto: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<AutomationProvenance>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<RunActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring_wake_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring_wake_cap_reached: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_attempts: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<ProviderQuotaBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_wait: Option<RoutingWait>,
    pub created_at: String,
    /// Most recent meaningful task activity. Read/unread changes do not advance it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub tokens_used: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_pull_request_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_issue_number_seeded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_origin: Option<TitleOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker_refs: Option<MarkerRefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_pr_candidates: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_issue_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_issue_candidates: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_reclaimed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_proc_count: Option<f64>,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub steps: Vec<StepState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_def: Option<WorkflowDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_intent: Option<RoutingIntent>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The title provenance enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleOrigin {
    User,
    Auto,
    Marker,
}

/// The marker reference pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkerRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<f64>,
}

/// `ApiRun` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiRun {
    #[serde(flatten)]
    pub record: RunRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProcessUsage>,
}

/// `ModelUsageEntry` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageEntry {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
    pub pct: f64,
}

/// Mirrors the per-project reference-status map in `RunsIndexResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceStatuses {
    pub prs: std::collections::BTreeMap<String, ReferenceStatus>,
    pub issues: std::collections::BTreeMap<String, ReferenceStatus>,
}

/// `RunIndexEntry` contract shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunIndexEntry {
    pub project_id: String,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_origin: Option<TitleOrigin>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<RunActivity>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// A bounded, whitespace-collapsed preview of the exact initial request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_at: Option<String>,
    pub workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_pull_request_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_issue_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker_refs: Option<MarkerRefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_proc_count: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProcessUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<Runner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<Vec<ModelUsageEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
}

/// `RunsIndexResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunsIndexResponse {
    pub runs: Vec<RunIndexEntry>,
    pub reference_statuses: std::collections::BTreeMap<String, ReferenceStatuses>,
    pub per_project_limit: u64,
    pub truncated: Vec<String>,
}

/// `CreateRunResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreateRunResponse {
    Single(Box<RunRecord>),
    Group { runs: Vec<RunRecord> },
}

/// `CancelResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResponse {
    pub cancelled: bool,
}

/// `CancelAutoResumeResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAutoResumeResponse {
    pub cancelled: bool,
}

/// `ArchiveFinishedResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveFinishedResponse {
    pub archived: f64,
}

/// `MarkAllReadResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkAllReadResponse {
    pub read: f64,
}

/// `DeleteRunResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRunResponse {
    pub deleted: bool,
}

/// `FinishResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishResponse {
    pub finished: bool,
}

/// `ContinueResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueResponse {
    pub continued: bool,
}

/// `CreatePrResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePrResponse {
    pub url: String,
    pub dry_run: bool,
}

/// `MessageResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageResponse {
    Delivered {
        delivered: bool,
    },
    Queued {
        queued: bool,
        message: QueuedMessage,
    },
    Deferred {
        deferred: bool,
    },
}

/// `EditQueuedMessageResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditQueuedMessageResponse {
    pub message: QueuedMessage,
}

/// `RemoveQueuedMessageResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveQueuedMessageResponse {
    pub removed: bool,
}

/// `OpenInCliResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInCliResponse {
    pub opened: bool,
    pub command: String,
}

/// `RemoveWorktreeResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWorktreeResponse {
    pub removed: bool,
}

/// `GitCommitResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitResponse {
    pub committed: bool,
    pub sha: String,
}

/// `GitPushResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushResponse {
    pub pushed: bool,
    pub branch: String,
    pub remote: String,
    pub upstream_set: bool,
}

/// `RunCommit` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub when: String,
}

/// `RunCommitsResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommitsResponse {
    pub commits: Vec<RunCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub pushed: bool,
}

/// `GroupVariant` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupVariant {
    pub id: String,
    pub variant: String,
    pub title: String,
    pub status: RunStatus,
    pub archived: bool,
    pub tokens_used: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub diff_stat: String,
    pub handoff_excerpt: String,
}

/// `GroupResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupResponse {
    pub group_id: String,
    pub runs: Vec<GroupVariant>,
}

/// `PickVariantResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PickVariantResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<RunRecord>,
}

/// The request body for selecting a winning run from a variant group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickVariantRequest {
    pub run_id: String,
}

/// `ImageInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInput {
    pub media_type: String,
    pub data: String,
}

/// `CreateRunInputBase` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateRunInputBase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<WorkflowStepDef>>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_auto: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
}

/// The refined create-run input has the same wire fields as its base schema.
pub type CreateRunInput = CreateRunInputBase;

/// `MessageInput` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
}

/// `PatchRunInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PatchRunInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Continue input. Every field is optional: an empty value reopens the last session on the
/// run's own backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContinueInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Request to open a project in a local target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInInput {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Request to create a Git commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitInput {
    pub message: String,
}

/// Patch for a queued message. An omitted field keeps its current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QueuedMessagePatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
}
