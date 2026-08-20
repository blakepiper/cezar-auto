use std::collections::BTreeMap;
use std::fmt;

use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::compat::ExtraFields;
use crate::health::{Runner, RunnerSelection};
use crate::reasoning::ReasoningEffort;

/// `WorkspaceConfigResponse` contract shape.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<u64>,
    pub autonomous: Option<bool>,
    pub worktree: Option<bool>,
    pub inherited_autonomous: InheritedAutonomous,
    pub inherited_worktree: bool,
    /// `true` commits and pushes at each natural checkpoint without asking; `false`/`None`
    /// (the hard default) leaves git actions to the user via the Task Git screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_auto: Option<bool>,
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

/// The workspace resource limits.
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

/// Providers accepted by quota routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaProvider {
    Claude,
    Codex,
    OpenCode,
}

/// Mirrors the quota-routing object in `WorkspaceConfigResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRouting {
    pub provider_order: Vec<QuotaProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_preference: Option<QualityPreference>,
    pub unknown_usage_policy: UnknownUsagePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_auto_attempts_per_generation: Option<u64>,
}

/// The unknown-usage policy enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownUsagePolicy {
    #[serde(rename = "allow_with_penalty", alias = "allow")]
    AllowWithPenalty,
    #[serde(rename = "exclude", alias = "deny")]
    Exclude,
}

/// The quality/cost preference used by automatic routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QualityPreference {
    Economy,
    Balanced,
    Best,
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

/// `SetWorkspaceConfigInput` contract shape.
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
    pub reasoning: Option<Option<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<Option<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Option<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_auto: Option<Option<bool>>,
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
#[serde(rename_all = "camelCase")]
pub struct QuotaRoutePolicyPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_eligible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u64>,
}

/// The partial quota-routing patch accepted by workspace settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRoutingPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_preference: Option<QualityPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_usage_policy: Option<UnknownUsagePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_auto_attempts_per_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<BTreeMap<String, QuotaRoutePolicyPatch>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<BTreeMap<String, QuotaRoutePolicyPatch>>,
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

/// `ProviderUsageSnapshot` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageSnapshot {
    pub provider: QuotaProvider,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_provider: Option<String>,
    pub health: ProviderUsageHealth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<UsageConfidence>,
    pub fetched_at: String,
    pub source: String,
    pub stale: bool,
    pub windows: Vec<ProviderUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumption: Option<UsageAggregate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderUsageError>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// How much trust Coducktor should place in a usage observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageConfidence {
    Authoritative,
    Observed,
    Inferred,
    Unknown,
}

/// Scope of locally recorded usage. Costs are never inferred across scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAggregateScope {
    CoducktorOnly,
    OpenCodeLocalHistory,
    ProviderAccount,
}

/// Sanitized token and cost consumption for one route or account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAggregate {
    pub scope: UsageAggregateScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Provider quota health.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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

/// `WorkspaceUsageResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceUsageResponse {
    pub providers: Vec<ProviderUsageSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<WorkspaceUsageRefresh>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_health: Option<WorkspaceUsagePolicyHealth>,
}

/// Overall refresh status for the cached usage view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUsageRefresh {
    pub refreshing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Small, sanitized health summary shared by Settings and headless usage output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUsagePolicyHealth {
    pub ready_candidates: u64,
    pub total_candidates: u64,
    pub unknown_candidates: u64,
}

/// The appearance bag shared by both UI-state files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Appearance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemePreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Accent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<ReadingWidth>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    Dark,
    #[serde(alias = "light")]
    Lazyvim,
    Lakes,
}

/// Appearance accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    Lime,
    Violet,
}

/// Appearance density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Comfortable,
    Compact,
    Ultra,
}

/// Appearance reading width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadingWidth {
    Narrow,
    Wide,
}

/// `TaskSource` contract shape.
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

/// `UiState` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task: Option<TaskSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_sources: Option<Vec<TaskSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_usage: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_view: Option<GithubView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_templates: Option<Vec<PromptTemplate>>,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// The GitHub tab view preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubView {
    Issues,
    Prs,
}

/// `WorkspaceLastLocation` contract shape.
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

/// `WorkspaceUiState` contract shape.
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

/// `RunnerModels` contract shape.
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

/// `ConfigResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    pub base_branch: Option<String>,
    pub default_runner: RunnerSelection,
    pub system_prompt: Option<String>,
    pub default_models: RunnerModels,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_defaults: Option<ProjectComposerDefaults>,
    pub models_locked: bool,
    pub max_parallel: u64,
    pub memory_limit_mb: Option<u64>,
    pub worktree_retention: u64,
    pub live_title_updates: Option<bool>,
    pub review_gate: Option<bool>,
}

/// The composer defaults explicitly configured by one project. Missing fields inherit from the
/// workspace composer defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectComposerDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_auto: Option<bool>,
}

/// The response to a config write has the same shape as `ConfigResponse`.
pub type SetConfigResponse = ConfigResponse;

/// `SetConfigInput` contract shape.
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
    pub composer_defaults: Option<ComposerDefaultsPatch>,
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

/// The provider id alias.
pub type ProviderId = Runner;

/// `ProviderConnectionState` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderConnectionState {
    Connected,
    Disconnected,
    NotInstalled,
    Unknown,
}

/// `ProviderStatus` contract shape.
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

/// `ProviderStatusResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatusResponse {
    pub providers: Vec<ProviderStatus>,
}

/// `ProviderConnectInput` contract shape.
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

/// `ProviderConnectResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderConnectResponse {
    AlreadyConnected(ProviderConnectAlreadyConnected),
    Opened(ProviderConnectOpened),
}

/// `ModelDiscoveryRunner` contract shape.
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

/// Whether a runner has a host-discovered model catalog.
pub fn runner_discovers_models(runner: Runner) -> bool {
    matches!(runner, Runner::Codex | Runner::OpenCode)
}

/// `RunnerModelOption` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerModelOption {
    pub id: String,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,
}

/// `RunnerModelCatalogResponse` contract shape.
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

/// `OpenTarget` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTarget {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `OpenTargetsResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTargetsResponse {
    pub targets: Vec<OpenTarget>,
}

/// `OpenProjectInRequest` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProjectInRequest {
    pub target: String,
}

/// `OpenProjectInResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProjectInResponse {
    pub opened: bool,
    pub path: String,
}
