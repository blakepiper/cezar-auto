use std::collections::BTreeMap;
use std::fmt;

use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::compat::ExtraFields;
use crate::health::{Runner, RunnerSelection};

/// Mirrors `packages/contract/src/workspace.ts::WorkspaceConfigResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfigResponse {
    pub projects_dir: String,
    pub composer_defaults: ComposerDefaults,
    pub resources: WorkspaceResources,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_routing: Option<QuotaRouting>,
    pub agent_defaults: AgentDefaults,
}

/// The composer defaults nested in the workspace configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerDefaults {
    pub autonomous: Option<bool>,
    pub worktree: Option<bool>,
    pub inherited_autonomous: InheritedAutonomous,
    pub inherited_worktree: bool,
}

/// The boolean-or-policy value used by `ComposerDefaults`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InheritedAutonomous {
    Value(bool),
    SourceDependent,
}

impl Serialize for InheritedAutonomous {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Value(value) => serializer.serialize_bool(*value),
            Self::SourceDependent => serializer.serialize_str("source-dependent"),
        }
    }
}

impl<'de> Deserialize<'de> for InheritedAutonomous {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct InheritedAutonomousVisitor;

        impl<'de> Visitor<'de> for InheritedAutonomousVisitor {
            type Value = InheritedAutonomous;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a boolean or source-dependent")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(InheritedAutonomous::Value(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if value == "source-dependent" {
                    Ok(InheritedAutonomous::SourceDependent)
                } else {
                    Err(E::custom("unknown autonomous inheritance policy"))
                }
            }
        }

        deserializer.deserialize_any(InheritedAutonomousVisitor)
    }
}

/// The workspace resource limits from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResources {
    pub max_parallel: u64,
    pub max_monitoring_sessions: u64,
    pub monitoring_wake_interval_minutes: Option<u64>,
    pub auto_resume_on_usage_limit: bool,
    pub intelligent_context_refresh: bool,
    pub memory_limit_mb: Option<u64>,
    pub worktree_retention_default: u64,
}

/// The two providers accepted by quota routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaProvider {
    Claude,
    Codex,
}

/// Mirrors the quota-routing object in `WorkspaceConfigResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRouting {
    pub enabled: bool,
    pub provider_order: [QuotaProvider; 2],
    pub unknown_usage_policy: UnknownUsagePolicy,
}

/// Mirrors the unknown-usage policy enum from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownUsagePolicy {
    Allow,
    Deny,
}

/// Mirrors the workspace machine-wide agent defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<RunnerModels>,
}

/// Mirrors `packages/contract/src/workspace.ts::SetWorkspaceConfigInput`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetWorkspaceConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_defaults: Option<ComposerDefaultsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_defaults: Option<AgentDefaultsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_routing: Option<QuotaRoutingPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<WorkspaceResourcesPatch>,
}

/// The partial composer patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerDefaultsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<Option<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Option<bool>>,
}

/// The partial agent-default patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaultsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<Option<RunnerSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<RunnerModelsPatch>,
}

/// The partial per-runner model patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunnerModelsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi: Option<Option<String>>,
}

/// The partial quota-routing patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuotaRoutingPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// The partial resource patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResourcesPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_monitoring_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring_wake_interval_minutes: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_on_usage_limit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intelligent_context_refresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_retention_default: Option<u64>,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderUsageSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageSnapshot {
    pub provider: QuotaProvider,
    pub profile_id: String,
    pub health: ProviderUsageHealth,
    pub fetched_at: String,
    pub source: String,
    pub stale: bool,
    pub windows: Vec<ProviderUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderUsageError>,
}

/// Provider quota health from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageHealth {
    Available,
    SoftExhausted,
    HardExhausted,
    AuthError,
    Unavailable,
    Unknown,
}

/// One provider quota window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageWindow {
    pub kind: ProviderUsageWindowKind,
    pub used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_limit_reached: Option<bool>,
}

/// Provider quota window kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderUsageWindowKind {
    Short,
    Long,
    Model,
    Unknown,
}

/// A sanitized provider quota error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsageError {
    pub code: String,
    pub message: String,
}

/// Mirrors `packages/contract/src/workspace.ts::WorkspaceUsageResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceUsageResponse {
    pub providers: Vec<ProviderUsageSnapshot>,
}

/// The appearance bag shared by both UI-state files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Appearance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Accent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<ReadingWidth>,
}

/// Appearance accent from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    Lime,
    Violet,
}

/// Appearance density from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Comfortable,
    Compact,
    Ultra,
}

/// Appearance reading width from `packages/contract/src/workspace.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadingWidth {
    Narrow,
    Wide,
}

/// Mirrors `packages/contract/src/workspace.ts::TaskSource`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum TaskSource {
    #[serde(rename = "baseline")]
    Baseline,
    #[serde(rename = "workflow")]
    Workflow {
        #[serde(rename = "ref")]
        reference: String,
    },
    #[serde(rename = "skill")]
    Skill {
        #[serde(rename = "ref")]
        reference: String,
    },
}

/// Mirrors the open task-table state bag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskTableUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_columns: Option<BTreeMap<String, bool>>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// A prompt template in the open UI-state bag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub id: String,
    pub label: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

/// Mirrors `packages/contract/src/workspace.ts::UiState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task: Option<TaskSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_sources: Option<Vec<TaskSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_autonomous: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_generate_followups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_usage: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_view: Option<RunsView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_view: Option<GithubView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_templates: Option<Vec<PromptTemplate>>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// The task list view preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunsView {
    List,
    Table,
}

/// The GitHub tab view preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubView {
    Issues,
    Prs,
}

/// Mirrors `packages/contract/src/workspace.ts::WorkspaceLastLocation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceLastLocation {
    pub project_id: String,
    pub pathname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// The legacy workspace sidebar state bag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SidebarUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<BTreeMap<String, bool>>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// The workspace-wide provider-auth dismissal bag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DismissedProviderAuthFailures {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi: Option<String>,
}

/// The open notifications state bag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NotificationsUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// Mirrors `packages/contract/src/workspace.ts::WorkspaceUiState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<SidebarUiState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_provider_auth_failures: Option<DismissedProviderAuthFailures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<NotificationsUiState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_table: Option<TaskTableUiState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_location: Option<WorkspaceLastLocation>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// The write-side workspace UI state uses the same open wire bag.
pub type SetWorkspaceUiStateInput = WorkspaceUiState;

/// Mirrors `packages/contract/src/workspace.ts::RunnerModels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunnerModels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi: Option<String>,
}

/// Mirrors `packages/contract/src/workspace.ts::ConfigResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    pub base_branch: Option<String>,
    pub default_runner: RunnerSelection,
    pub system_prompt: Option<String>,
    pub default_models: RunnerModels,
    pub models_locked: bool,
    pub max_parallel: u64,
    pub memory_limit_mb: Option<u64>,
    pub worktree_retention: u64,
    pub live_title_updates: Option<bool>,
    pub review_gate: Option<bool>,
}

/// The response to a config write has the same shape as `ConfigResponse`.
pub type SetConfigResponse = ConfigResponse;

/// Mirrors `packages/contract/src/workspace.ts::SetConfigInput`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_models: Option<RunnerModelsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_retention: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_title_updates: Option<Option<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_gate: Option<Option<bool>>,
}

/// The provider id alias from `packages/contract/src/workspace.ts`.
pub type ProviderId = Runner;

/// Mirrors `packages/contract/src/workspace.ts::ProviderConnectionState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderConnectionState {
    Connected,
    Disconnected,
    NotInstalled,
    Unknown,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderStatus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub provider: ProviderId,
    pub status: ProviderConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_failure_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderStatusResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatusResponse {
    pub providers: Vec<ProviderStatus>,
}

/// Mirrors the optional query accepted by `GET /api/v1/providers/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderStatusQuery {
    #[serde(default)]
    pub refresh: Option<String>,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderEnabledInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEnabledInput {
    pub enabled: bool,
}

/// Mirrors the current-auth incident body accepted by the retry route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetryInput {
    pub auth_failure_id: String,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderConnectInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnectInput {
    pub provider: Runner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// The successful provider-connect branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConnectOpened {
    pub opened: bool,
    pub command: String,
}

/// The already-connected provider-connect branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConnectAlreadyConnected {
    pub opened: bool,
    pub connected: bool,
    pub command: String,
}

/// Mirrors `packages/contract/src/workspace.ts::ProviderConnectResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderConnectResponse {
    AlreadyConnected(ProviderConnectAlreadyConnected),
    Opened(ProviderConnectOpened),
}

/// Mirrors `packages/contract/src/workspace.ts::ModelDiscoveryRunner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDiscoveryRunner {
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
}

/// The host-discovered model runners.
pub const MODEL_DISCOVERY_RUNNERS: [ModelDiscoveryRunner; 2] =
    [ModelDiscoveryRunner::Codex, ModelDiscoveryRunner::OpenCode];

/// The query accepted by `GET /api/v1/models`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDiscoveryQuery {
    pub runner: ModelDiscoveryRunner,
}

/// Whether a runner has a host-discovered model catalog.
pub fn runner_discovers_models(runner: Runner) -> bool {
    matches!(runner, Runner::Codex | Runner::OpenCode)
}

/// Mirrors `packages/contract/src/workspace.ts::RunnerModelOption`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerModelOption {
    pub id: String,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,
}

/// Mirrors `packages/contract/src/workspace.ts::RunnerModelCatalogResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerModelCatalogResponse {
    pub runner: Runner,
    pub models: Vec<RunnerModelOption>,
    pub source: ModelCatalogSource,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The source of a discovered model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCatalogSource {
    Live,
    Cache,
    Unavailable,
}

/// Mirrors `packages/contract/src/workspace.ts::OpenTarget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTarget {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// Mirrors `packages/contract/src/workspace.ts::OpenTargetsResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTargetsResponse {
    pub targets: Vec<OpenTarget>,
}

/// Mirrors `packages/contract/src/workspace.ts::OpenProjectInRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProjectInRequest {
    pub target: String,
}

/// Mirrors `packages/contract/src/workspace.ts::OpenProjectInResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProjectInResponse {
    pub opened: bool,
    pub path: String,
}
