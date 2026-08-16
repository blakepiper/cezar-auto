use serde::{Deserialize, Serialize};

use crate::github::ReferenceStatus;
use crate::health::{Runner, RunnerSelection};
use crate::reasoning::{ConcreteReasoningEffort, ReasoningEffort};
use crate::workflows::{WorkflowDef, WorkflowStepDef};
use crate::workspace::QuotaProvider;

/// Mirrors `packages/contract/src/runs.ts::RunStatus`.
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

/// Mirrors `packages/contract/src/runs.ts::RunActivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunActivity {
    Monitoring,
}

/// Mirrors `packages/contract/src/runs.ts::ProviderQuotaBlockedReason`.
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

/// Mirrors `packages/contract/src/runs.ts::StepStatus`.
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

/// Mirrors `packages/contract/src/runs.ts::StepState`.
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
}

/// The agent/check discriminator in a step state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Agent,
    Check,
}

/// Mirrors `packages/contract/src/runs.ts::DiffStat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub adds: f64,
    pub dels: f64,
    pub files: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repointed: Option<bool>,
}

/// Mirrors `packages/contract/src/runs.ts::QueuedMessage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    pub created_at: String,
}

/// Mirrors `packages/contract/src/runs.ts::ProcessUsage`.
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

/// Mirrors `packages/contract/src/runs.ts::RunRecord`.
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
    pub generate_followups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
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
    pub created_at: String,
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
}

/// The title provenance enum from `packages/contract/src/runs.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleOrigin {
    User,
    Auto,
    Marker,
}

/// The marker reference pair from `packages/contract/src/runs.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkerRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<f64>,
}

/// Mirrors `packages/contract/src/runs.ts::ApiRun`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiRun {
    #[serde(flatten)]
    pub record: RunRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProcessUsage>,
}

/// Mirrors `packages/contract/src/runs.ts::ModelUsageEntry`.
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

/// Mirrors `packages/contract/src/runs.ts::RunIndexEntry`.
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
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
    pub archived: bool,
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

/// Mirrors `packages/contract/src/runs.ts::RunsIndexResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunsIndexResponse {
    pub runs: Vec<RunIndexEntry>,
    pub reference_statuses: std::collections::BTreeMap<String, ReferenceStatuses>,
    pub per_project_limit: u64,
    pub truncated: Vec<String>,
}

/// Mirrors `packages/contract/src/runs.ts::CreateRunResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreateRunResponse {
    Single(Box<RunRecord>),
    Group { runs: Vec<RunRecord> },
}

/// Mirrors `packages/contract/src/runs.ts::CancelResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResponse {
    pub cancelled: bool,
}

/// Mirrors `packages/contract/src/runs.ts::CancelAutoResumeResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAutoResumeResponse {
    pub cancelled: bool,
}

/// Mirrors `packages/contract/src/runs.ts::ArchiveFinishedResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveFinishedResponse {
    pub archived: f64,
}

/// Mirrors `packages/contract/src/runs.ts::MarkAllReadResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkAllReadResponse {
    pub read: f64,
}

/// Mirrors `packages/contract/src/runs.ts::DeleteRunResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRunResponse {
    pub deleted: bool,
}

/// Mirrors `packages/contract/src/runs.ts::FinishResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishResponse {
    pub finished: bool,
}

/// Mirrors `packages/contract/src/runs.ts::ContinueResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueResponse {
    pub continued: bool,
}

/// Mirrors `packages/contract/src/runs.ts::CreatePrResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePrResponse {
    pub url: String,
    pub dry_run: bool,
}

/// Mirrors `packages/contract/src/runs.ts::MessageResponse`.
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

/// Mirrors `packages/contract/src/runs.ts::EditQueuedMessageResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditQueuedMessageResponse {
    pub message: QueuedMessage,
}

/// Mirrors `packages/contract/src/runs.ts::RemoveQueuedMessageResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveQueuedMessageResponse {
    pub removed: bool,
}

/// Mirrors `packages/contract/src/runs.ts::OpenInCliResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInCliResponse {
    pub opened: bool,
    pub command: String,
}

/// Mirrors `packages/contract/src/runs.ts::RemoveWorktreeResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWorktreeResponse {
    pub removed: bool,
}

/// Mirrors `packages/contract/src/runs.ts::GitCommitResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitResponse {
    pub committed: bool,
    pub sha: String,
}

/// Mirrors `packages/contract/src/runs.ts::GitPushResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushResponse {
    pub pushed: bool,
    pub branch: String,
    pub remote: String,
    pub upstream_set: bool,
}

/// Mirrors `packages/contract/src/runs.ts::RunCommit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub when: String,
}

/// Mirrors `packages/contract/src/runs.ts::RunCommitsResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommitsResponse {
    pub commits: Vec<RunCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub pushed: bool,
}

/// Mirrors `packages/contract/src/runs.ts::GroupVariant`.
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

/// Mirrors `packages/contract/src/runs.ts::GroupResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupResponse {
    pub group_id: String,
    pub runs: Vec<GroupVariant>,
}

/// Mirrors `packages/contract/src/runs.ts::PickVariantResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PickVariantResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<RunRecord>,
}

/// The `POST /groups/:groupId/pick` request body (`pickSchema` in `server.ts`) — not exposed
/// as a named contract type on the TS side (the route validates an inline `z.object`), but the
/// Rust client needs a typed body the same way every other write does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickVariantRequest {
    pub run_id: String,
}

/// Mirrors `packages/contract/src/runs.ts::ImageInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInput {
    pub media_type: String,
    pub data: String,
}

/// Mirrors `packages/contract/src/runs.ts::CreateRunInputBase`.
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
    pub generate_followups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_id: Option<String>,
}

/// The refined create-run input has the same wire fields as its base schema.
pub type CreateRunInput = CreateRunInputBase;

/// Mirrors `packages/contract/src/runs.ts::MessageInput`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
}

/// Mirrors `packages/contract/src/runs.ts::PatchRunInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PatchRunInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Mirrors `continueSchema` in `packages/cezar/src/server/server.ts` (§8.4 "Continue").
/// Every field optional: an empty body reopens the last session on the run's own
/// backend, exactly as `POST /runs/:id/continue` with no body does today.
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

/// Mirrors `openInSchema` in `packages/cezar/src/server/server.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInInput {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Mirrors `gitCommitSchema` in `packages/cezar/src/server/server.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitInput {
    pub message: String,
}

/// Mirrors `queuedMessagePatchSchema` in `packages/cezar/src/server/server.ts`. PATCH
/// semantics: an omitted field keeps its current value — the cockpit edits text
/// without re-uploading existing attachments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QueuedMessagePatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageInput>>,
}
