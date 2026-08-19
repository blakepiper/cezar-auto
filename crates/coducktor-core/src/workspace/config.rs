//! `~/.coducktor/config.json` — the per-user workspace config and project registry. House rules:
//!
//! - every field optional/defaulted with a per-key catch, so a bad value degrades in
//!   place instead of discarding the file (contrast `crate::config`, whose per-repo
//!   schema fails the whole object on seven of its fields — this file never does that);
//! - every object level round-trips unknown keys (`.passthrough()`), so a key a *newer*
//!   coducktor wrote survives a round-trip through an older one;
//! - atomic tmp+rename writes with mode `0600` (dir `0700`);
//! - a corrupt file degrades to in-memory defaults plus one caller-visible warning — the
//!   registry rebuilds as projects are opened, so losing it is an inconvenience, not data
//!   loss. The corrupt file is left in place until the next successful merge-write
//!   replaces it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::{Map, Value};

use coducktor_contract::workspace::{QualityPreference, QuotaProvider, UnknownUsagePolicy};
use coducktor_contract::{ReasoningEffort, Runner, RunnerSelection};

use crate::paths::EnvSource;
use crate::zod;

/// `id` slug rule — mirrors `PROJECT_ID_RE = /^[a-z0-9][a-z0-9-]{0,63}$/` and the
/// identical rule `workspace::agent_accounts::AGENT_ACCOUNT_ID_RE` reuses.
pub fn is_valid_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let is_head = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let is_tail = |b: u8| is_head(b) || b == b'-';
    is_head(bytes[0]) && bytes[1..].iter().all(|&b| is_tail(b))
}

/// The four known agent backends, in the canonical order `disabled_providers` re-sorts
/// to.
pub const PROVIDER_IDS: [Runner; 4] = [Runner::Claude, Runner::Codex, Runner::OpenCode, Runner::Pi];

fn runner_value(runner: Runner) -> Value {
    Value::String(
        match runner {
            Runner::Claude => "claude",
            Runner::Codex => "codex",
            Runner::OpenCode => "opencode",
            Runner::Pi => "pi",
        }
        .to_owned(),
    )
}

fn runner_selection_value(runner: RunnerSelection) -> Value {
    Value::String(
        match runner {
            RunnerSelection::Claude => "claude",
            RunnerSelection::Codex => "codex",
            RunnerSelection::OpenCode => "opencode",
            RunnerSelection::Pi => "pi",
            RunnerSelection::Auto => "auto",
        }
        .to_owned(),
    )
}

fn quota_provider_value(provider: QuotaProvider) -> Value {
    Value::String(
        match provider {
            QuotaProvider::Claude => "claude",
            QuotaProvider::Codex => "codex",
            QuotaProvider::OpenCode => "opencode",
        }
        .to_owned(),
    )
}

fn unknown_usage_policy_value(policy: UnknownUsagePolicy) -> Value {
    Value::String(
        match policy {
            UnknownUsagePolicy::AllowWithPenalty => "allow_with_penalty",
            UnknownUsagePolicy::Exclude => "exclude",
        }
        .to_owned(),
    )
}

fn quality_preference_value(preference: QualityPreference) -> Value {
    Value::String(
        match preference {
            QualityPreference::Economy => "economy",
            QualityPreference::Balanced => "balanced",
            QualityPreference::Best => "best",
        }
        .to_owned(),
    )
}

/// One registry entry. `id` and
/// `root` are load-bearing — an entry missing either is dropped by the caller's
/// per-entry salvage; every other field degrades to its own default in place.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceProject {
    pub id: String,
    pub root: String,
    pub name: String,
    pub added_at: String,
    pub last_opened_at: String,
    pub source: ProjectSource,
    pub max_parallel: Option<u64>,
    pub tags: Option<Vec<String>>,
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSource {
    Local,
    Checkout,
}

const PROJECT_TAG_MAX_LENGTH: usize = coducktor_contract::PROJECT_TAG_MAX_LENGTH;
const PROJECT_TAGS_MAX: usize = coducktor_contract::PROJECT_TAGS_MAX;

const PROJECT_KEYS: &[&str] = &[
    "id",
    "root",
    "name",
    "addedAt",
    "lastOpenedAt",
    "source",
    "maxParallel",
    "tags",
];

impl WorkspaceProject {
    /// `None` when `id` or `root` fails validation — the per-entry salvage drops the
    /// whole row rather than materializing a project with no identity.
    fn parse(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = zod::regex_str(object.get("id"), is_valid_slug)?.to_owned();
        let root = object
            .get("root")
            .and_then(Value::as_str)
            .filter(|s| {
                let len = s.chars().count();
                (1..=4096).contains(&len) && s.starts_with('/')
            })?
            .to_owned();
        let source = match object.get("source").and_then(Value::as_str) {
            Some("checkout") => ProjectSource::Checkout,
            _ => ProjectSource::Local,
        };
        let tags = object
            .get("tags")
            .and_then(Value::as_array)
            .and_then(|entries| {
                let tags: Option<Vec<String>> = entries
                    .iter()
                    .map(|entry| {
                        let raw = entry.as_str()?.trim();
                        let len = raw.chars().count();
                        (1..=PROJECT_TAG_MAX_LENGTH)
                            .contains(&len)
                            .then(|| raw.to_owned())
                    })
                    .collect();
                tags.filter(|t| t.len() <= PROJECT_TAGS_MAX)
            });
        Some(Self {
            id,
            root,
            name: zod::capped_str_or(object.get("name"), 200, ""),
            added_at: zod::capped_str_or(object.get("addedAt"), 64, ""),
            last_opened_at: zod::capped_str_or(object.get("lastOpenedAt"), 64, ""),
            source,
            max_parallel: zod::bounded_i64_nullable(object.get("maxParallel"), 1, 16, None)
                .map(|v| v as u64),
            tags,
            extra: zod::extra_fields(object, PROJECT_KEYS),
        })
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("id", Value::String(self.id.clone())),
                ("root", Value::String(self.root.clone())),
                ("name", Value::String(self.name.clone())),
                ("addedAt", Value::String(self.added_at.clone())),
                ("lastOpenedAt", Value::String(self.last_opened_at.clone())),
                (
                    "source",
                    Value::String(match self.source {
                        ProjectSource::Local => "local".to_owned(),
                        ProjectSource::Checkout => "checkout".to_owned(),
                    }),
                ),
                (
                    "maxParallel",
                    self.max_parallel.map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "tags",
                    match &self.tags {
                        Some(tags) => Value::from(tags.clone()),
                        None => Value::Null,
                    },
                ),
            ],
        )
    }
}

/// Zero-config cadence, in minutes, for re-checking a run parked with `DUCK:MONITORING`.
/// Default monitoring wake interval.
pub const DEFAULT_MONITORING_WAKE_MINUTES: u64 = 5;

/// Workspace resource limits and defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Resources {
    pub max_parallel: u64,
    pub max_monitoring_sessions: u64,
    /// `None` is an explicit "park until resumed" choice (the schema's nullable), and is
    /// preserved distinct from the default — never replaced by it.
    pub monitoring_wake_interval_minutes: Option<u64>,
    pub auto_resume_on_usage_limit: bool,
    pub intelligent_context_refresh: bool,
    pub memory_limit_mb: Option<u64>,
    pub worktree_retention_default: u64,
    pub extra: Map<String, Value>,
}

const RESOURCES_KEYS: &[&str] = &[
    "maxParallel",
    "maxMonitoringSessions",
    "monitoringWakeIntervalMinutes",
    "autoResumeOnUsageLimit",
    "intelligentContextRefresh",
    "memoryLimitMb",
    "worktreeRetentionDefault",
];

impl Default for Resources {
    fn default() -> Self {
        Self::parse(None)
    }
}

impl Resources {
    fn parse(value: Option<&Value>) -> Self {
        let object = zod::as_map(value);
        Self {
            max_parallel: zod::bounded_i64(zod::field(object, "maxParallel"), 1, 16, 2) as u64,
            max_monitoring_sessions: zod::bounded_i64(
                zod::field(object, "maxMonitoringSessions"),
                0,
                16,
                2,
            ) as u64,
            monitoring_wake_interval_minutes: zod::bounded_i64_nullable(
                zod::field(object, "monitoringWakeIntervalMinutes"),
                1,
                60,
                Some(DEFAULT_MONITORING_WAKE_MINUTES as i64),
            )
            .map(|v| v as u64),
            auto_resume_on_usage_limit: zod::bool_or(
                zod::field(object, "autoResumeOnUsageLimit"),
                true,
            ),
            intelligent_context_refresh: zod::bool_or(
                zod::field(object, "intelligentContextRefresh"),
                false,
            ),
            memory_limit_mb: zod::bounded_i64_nullable(
                zod::field(object, "memoryLimitMb"),
                0,
                1_048_576,
                None,
            )
            .map(|v| v as u64),
            worktree_retention_default: zod::bounded_i64(
                zod::field(object, "worktreeRetentionDefault"),
                0,
                1000,
                10,
            ) as u64,
            extra: object
                .map(|o| zod::extra_fields(o, RESOURCES_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("maxParallel", Value::from(self.max_parallel)),
                (
                    "maxMonitoringSessions",
                    Value::from(self.max_monitoring_sessions),
                ),
                (
                    "monitoringWakeIntervalMinutes",
                    self.monitoring_wake_interval_minutes
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                ),
                (
                    "autoResumeOnUsageLimit",
                    Value::from(self.auto_resume_on_usage_limit),
                ),
                (
                    "intelligentContextRefresh",
                    Value::from(self.intelligent_context_refresh),
                ),
                (
                    "memoryLimitMb",
                    self.memory_limit_mb.map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "worktreeRetentionDefault",
                    Value::from(self.worktree_retention_default),
                ),
            ],
        )
    }
}

/// Workspace composer defaults.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposerDefaults {
    pub reasoning: Option<ReasoningEffort>,
    pub variants: Option<u64>,
    pub autonomous: Option<bool>,
    pub worktree: Option<bool>,
    pub extra: Map<String, Value>,
}

const COMPOSER_DEFAULTS_KEYS: &[&str] = &["reasoning", "variants", "autonomous", "worktree"];

impl ComposerDefaults {
    fn parse(value: Option<&Value>) -> Self {
        let object = zod::as_map(value);
        Self {
            reasoning: zod::field(object, "reasoning")
                .and_then(|value| serde_json::from_value(value.clone()).ok()),
            variants: zod::field(object, "variants").and_then(|value| {
                let variants = zod::bounded_i64(Some(value), 1, 3, 0);
                (1..=3).contains(&variants).then_some(variants as u64)
            }),
            autonomous: zod::bool_opt(zod::field(object, "autonomous")),
            worktree: zod::bool_opt(zod::field(object, "worktree")),
            extra: object
                .map(|o| zod::extra_fields(o, COMPOSER_DEFAULTS_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                (
                    "reasoning",
                    self.reasoning
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                        .unwrap_or(Value::Null),
                ),
                (
                    "variants",
                    self.variants.map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "autonomous",
                    self.autonomous.map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "worktree",
                    self.worktree.map(Value::from).unwrap_or(Value::Null),
                ),
            ],
        )
    }
}

/// The per-runner model preset bag nested in `agentDefaults` — unlike `crate::config`'s
/// `defaultModels` (no passthrough), this one carries `.passthrough()` too, so it needs
/// its own `extra` bucket.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDefaultModels {
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub opencode: Option<String>,
    pub pi: Option<String>,
    pub extra: Map<String, Value>,
}

const AGENT_DEFAULT_MODELS_KEYS: &[&str] = &["claude", "codex", "opencode", "pi"];

impl AgentDefaultModels {
    fn parse(value: Option<&Value>) -> Option<Self> {
        let object = zod::as_map(value)?;
        Some(Self {
            claude: zod::trimmed_str_opt(object.get("claude"), 1, 200),
            codex: zod::trimmed_str_opt(object.get("codex"), 1, 200),
            opencode: zod::trimmed_str_opt(object.get("opencode"), 1, 200),
            pi: zod::trimmed_str_opt(object.get("pi"), 1, 200),
            extra: zod::extra_fields(object, AGENT_DEFAULT_MODELS_KEYS),
        })
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                (
                    "claude",
                    self.claude.clone().map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "codex",
                    self.codex.clone().map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "opencode",
                    self.opencode
                        .clone()
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                ),
                (
                    "pi",
                    self.pi.clone().map(Value::from).unwrap_or(Value::Null),
                ),
            ],
        )
    }
}

/// Agent defaults — what a repo that has said
/// nothing runs. Every key optional with NO default: an absent `runner` must stay
/// distinguishable from one someone chose, or "fall back to the machine default"
/// collapses into "always claude".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDefaults {
    pub runner: Option<RunnerSelection>,
    pub models: Option<AgentDefaultModels>,
    pub extra: Map<String, Value>,
}

const AGENT_DEFAULTS_KEYS: &[&str] = &["runner", "models"];

impl AgentDefaults {
    fn parse(value: Option<&Value>) -> Self {
        let object = zod::as_map(value);
        Self {
            runner: zod::field(object, "runner")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            models: AgentDefaultModels::parse(zod::field(object, "models")),
            extra: object
                .map(|o| zod::extra_fields(o, AGENT_DEFAULTS_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                (
                    "runner",
                    self.runner
                        .map(runner_selection_value)
                        .unwrap_or(Value::Null),
                ),
                (
                    "models",
                    self.models
                        .as_ref()
                        .map(AgentDefaultModels::to_value)
                        .unwrap_or(Value::Null),
                ),
            ],
        )
    }
}

/// Per-provider quota policy.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaProviderPolicy {
    pub enabled: bool,
    pub priority: u64,
    pub stop_new_work_at_percent: f64,
    pub long_window_stop_at_percent: f64,
    pub resume_below_percent: f64,
    pub max_concurrent_per_account: u64,
    legacy_max_concurrent_key: bool,
    pub extra: Map<String, Value>,
}

const QUOTA_PROVIDER_POLICY_KEYS: &[&str] = &[
    "enabled",
    "priority",
    "stopNewWorkAtPercent",
    "longWindowStopAtPercent",
    "resumeBelowPercent",
    "maxConcurrentPerAccount",
    // Read-only compatibility spelling used by the first quota scaffold.
    "maxConcurrent",
];

impl QuotaProviderPolicy {
    /// Provider-specific defaults keep the initial policy stable while allowing the config file
    /// to override every routing preference explicitly.
    fn parse(value: Option<&Value>, long_window_default: f64, priority_default: u64) -> Self {
        let object = zod::as_map(value);
        let max_concurrent = zod::field(object, "maxConcurrentPerAccount")
            .or_else(|| zod::field(object, "maxConcurrent"));
        Self {
            enabled: zod::bool_or(zod::field(object, "enabled"), true),
            priority: zod::bounded_i64(
                zod::field(object, "priority"),
                0,
                10_000,
                priority_default as i64,
            ) as u64,
            stop_new_work_at_percent: zod::bounded_f64(
                zod::field(object, "stopNewWorkAtPercent"),
                0.0,
                100.0,
                90.0,
            ),
            long_window_stop_at_percent: zod::bounded_f64(
                zod::field(object, "longWindowStopAtPercent"),
                0.0,
                100.0,
                long_window_default,
            ),
            resume_below_percent: zod::bounded_f64(
                zod::field(object, "resumeBelowPercent"),
                0.0,
                100.0,
                80.0,
            ),
            max_concurrent_per_account: zod::bounded_i64(max_concurrent, 1, 16, 1) as u64,
            legacy_max_concurrent_key: object.is_some_and(|object| {
                object.contains_key("maxConcurrent")
                    && !object.contains_key("maxConcurrentPerAccount")
            }),
            extra: object
                .map(|o| zod::extra_fields(o, QUOTA_PROVIDER_POLICY_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("enabled", Value::from(self.enabled)),
                ("priority", Value::from(self.priority)),
                (
                    "stopNewWorkAtPercent",
                    Value::from(self.stop_new_work_at_percent),
                ),
                (
                    "longWindowStopAtPercent",
                    Value::from(self.long_window_stop_at_percent),
                ),
                ("resumeBelowPercent", Value::from(self.resume_below_percent)),
                if self.legacy_max_concurrent_key {
                    (
                        "maxConcurrent",
                        Value::from(self.max_concurrent_per_account),
                    )
                } else {
                    (
                        "maxConcurrentPerAccount",
                        Value::from(self.max_concurrent_per_account),
                    )
                },
            ],
        )
    }
}

/// Per-account or per-route automatic eligibility. The key is a user-authored profile or route
/// identity, so it is retained even when the corresponding installation is currently absent.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaRoutePolicy {
    pub auto_eligible: bool,
    pub priority: u64,
    pub extra: Map<String, Value>,
}

const QUOTA_ROUTE_POLICY_KEYS: &[&str] = &["autoEligible", "priority"];

impl QuotaRoutePolicy {
    fn parse(value: &Value) -> Self {
        let object = value.as_object();
        Self {
            auto_eligible: zod::bool_or(zod::field(object, "autoEligible"), false),
            priority: zod::bounded_i64(zod::field(object, "priority"), 0, 10_000, 50) as u64,
            extra: object
                .map(|o| zod::extra_fields(o, QUOTA_ROUTE_POLICY_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("autoEligible", Value::from(self.auto_eligible)),
                ("priority", Value::from(self.priority)),
            ],
        )
    }
}

fn parse_quota_route_policies(value: Option<&Value>) -> BTreeMap<String, QuotaRoutePolicy> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter(|(key, _)| key.chars().count() <= 256)
                .map(|(key, value)| (key.clone(), QuotaRoutePolicy::parse(value)))
                .collect()
        })
        .unwrap_or_default()
}

fn quota_route_policies_to_value(policies: &BTreeMap<String, QuotaRoutePolicy>) -> Value {
    Value::Object(
        policies
            .iter()
            .map(|(key, policy)| (key.clone(), policy.to_value()))
            .collect(),
    )
}

/// Policy used by per-run quota-aware `auto` selection.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaRouting {
    pub provider_order: Vec<QuotaProvider>,
    pub refresh_interval_seconds: u64,
    pub cache_ttl_seconds: u64,
    pub request_timeout_seconds: u64,
    pub quality_preference: QualityPreference,
    pub unknown_usage_policy: UnknownUsagePolicy,
    pub max_auto_attempts_per_generation: u64,
    pub claude: QuotaProviderPolicy,
    pub codex: QuotaProviderPolicy,
    pub opencode: QuotaProviderPolicy,
    pub accounts: BTreeMap<String, QuotaRoutePolicy>,
    pub routes: BTreeMap<String, QuotaRoutePolicy>,
    pub extra: Map<String, Value>,
    providers_extra: Map<String, Value>,
}

const DEFAULT_QUOTA_PROVIDER_ORDER: &[QuotaProvider] = &[
    QuotaProvider::Claude,
    QuotaProvider::Codex,
    QuotaProvider::OpenCode,
];

const QUOTA_ROUTING_KEYS: &[&str] = &[
    "providerOrder",
    "refreshIntervalSeconds",
    "cacheTtlSeconds",
    "requestTimeoutSeconds",
    "qualityPreference",
    "unknownUsagePolicy",
    "maxAutoAttemptsPerGeneration",
    "providers",
    "accounts",
    "routes",
];

impl Default for QuotaRouting {
    fn default() -> Self {
        Self::parse(None)
    }
}

impl QuotaRouting {
    fn parse(value: Option<&Value>) -> Self {
        let object = zod::as_map(value);
        let provider_order = zod::field(object, "providerOrder")
            .and_then(Value::as_array)
            .and_then(|entries| {
                let mut parsed = Vec::new();
                for entry in entries {
                    let provider = serde_json::from_value(entry.clone()).ok()?;
                    if !parsed.contains(&provider) {
                        parsed.push(provider);
                    }
                }
                (!parsed.is_empty()).then_some(parsed)
            })
            .unwrap_or_else(|| DEFAULT_QUOTA_PROVIDER_ORDER.to_vec());
        let providers = zod::as_map(zod::field(object, "providers"));
        Self {
            provider_order,
            refresh_interval_seconds: zod::bounded_i64(
                zod::field(object, "refreshIntervalSeconds"),
                5,
                3600,
                60,
            ) as u64,
            cache_ttl_seconds: zod::bounded_i64(zod::field(object, "cacheTtlSeconds"), 1, 3600, 30)
                as u64,
            request_timeout_seconds: zod::bounded_i64(
                zod::field(object, "requestTimeoutSeconds"),
                1,
                60,
                8,
            ) as u64,
            quality_preference: zod::field(object, "qualityPreference")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(QualityPreference::Balanced),
            unknown_usage_policy: zod::field(object, "unknownUsagePolicy")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(UnknownUsagePolicy::AllowWithPenalty),
            max_auto_attempts_per_generation: zod::bounded_i64(
                zod::field(object, "maxAutoAttemptsPerGeneration"),
                1,
                16,
                3,
            ) as u64,
            claude: QuotaProviderPolicy::parse(providers.and_then(|p| p.get("claude")), 95.0, 100),
            codex: QuotaProviderPolicy::parse(providers.and_then(|p| p.get("codex")), 90.0, 95),
            opencode: QuotaProviderPolicy::parse(
                providers.and_then(|p| p.get("opencode")),
                90.0,
                80,
            ),
            accounts: parse_quota_route_policies(zod::field(object, "accounts")),
            routes: parse_quota_route_policies(zod::field(object, "routes")),
            providers_extra: providers
                .map(|p| zod::extra_fields(p, &["claude", "codex", "opencode"]))
                .unwrap_or_default(),
            extra: object
                .map(|o| zod::extra_fields(o, QUOTA_ROUTING_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        let providers = zod::merge_extra(
            &self.providers_extra,
            vec![
                ("claude", self.claude.to_value()),
                ("codex", self.codex.to_value()),
                ("opencode", self.opencode.to_value()),
            ],
        );
        zod::merge_extra(
            &self.extra,
            vec![
                (
                    "providerOrder",
                    Value::from(
                        self.provider_order
                            .iter()
                            .map(|p| quota_provider_value(*p))
                            .collect::<Vec<_>>(),
                    ),
                ),
                (
                    "refreshIntervalSeconds",
                    Value::from(self.refresh_interval_seconds),
                ),
                ("cacheTtlSeconds", Value::from(self.cache_ttl_seconds)),
                (
                    "requestTimeoutSeconds",
                    Value::from(self.request_timeout_seconds),
                ),
                (
                    "qualityPreference",
                    quality_preference_value(self.quality_preference),
                ),
                (
                    "unknownUsagePolicy",
                    unknown_usage_policy_value(self.unknown_usage_policy),
                ),
                (
                    "maxAutoAttemptsPerGeneration",
                    Value::from(self.max_auto_attempts_per_generation),
                ),
                ("providers", providers),
                ("accounts", quota_route_policies_to_value(&self.accounts)),
                ("routes", quota_route_policies_to_value(&self.routes)),
            ],
        )
    }
}

const WORKSPACE_CONFIG_KEYS: &[&str] = &[
    "schemaVersion",
    "projectsDir",
    "modelsLocked",
    "resources",
    "composerDefaults",
    "disabledProviders",
    "agentDefaults",
    "quotaRouting",
    "projects",
];

/// The durable workspace configuration in `~/.coducktor/config.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceConfig {
    /// Migration cursor (`workspace::migrations`). Absent/bad → 0, meaning "run every
    /// migration" — safe because every migration is idempotent.
    pub schema_version: u32,
    /// Checkout root for GUI-cloned projects. Stored as written (a literal `~` is
    /// expanded by the checkout flow, not here).
    pub projects_dir: String,
    pub models_locked: Option<bool>,
    pub resources: Resources,
    pub composer_defaults: ComposerDefaults,
    /// Host-wide provider preferences; empty means every provider is enabled. Always
    /// re-sorted to `PROVIDER_IDS`' canonical order, deduplicated — never the input order.
    pub disabled_providers: Vec<Runner>,
    pub agent_defaults: AgentDefaults,
    pub quota_routing: QuotaRouting,
    /// Per-entry salvage: a corrupt entry is dropped, the rest of the registry survives.
    pub projects: Vec<WorkspaceProject>,
    pub extra: Map<String, Value>,
}

impl WorkspaceConfig {
    /// The in-memory default — what a missing or corrupt file behaves like.
    pub fn default_for(env: &dyn EnvSource) -> Self {
        Self::parse(&Value::Object(Default::default()), env)
    }

    fn parse(raw: &Value, env: &dyn EnvSource) -> Self {
        let object = raw.as_object();
        let projects_dir = zod::trimmed_str_opt(zod::field(object, "projectsDir"), 1, 4096)
            .unwrap_or_else(|| {
                env.get("DUCK_PROJECTS_DIR")
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "~/coducktor/projects".to_owned())
            });
        let disabled_providers = zod::field(object, "disabledProviders")
            .and_then(Value::as_array)
            .map(|entries| {
                let matched: Vec<Runner> = entries
                    .iter()
                    .filter_map(|v| serde_json::from_value::<Runner>(v.clone()).ok())
                    .collect();
                PROVIDER_IDS
                    .into_iter()
                    .filter(|p| matched.contains(p))
                    .collect()
            })
            .unwrap_or_default();
        let projects = zod::field(object, "projects")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(WorkspaceProject::parse).collect())
            .unwrap_or_default();
        Self {
            schema_version: zod::bounded_i64(zod::field(object, "schemaVersion"), 0, i64::MAX, 0)
                as u32,
            projects_dir,
            models_locked: zod::bool_opt(zod::field(object, "modelsLocked")),
            resources: Resources::parse(zod::field(object, "resources")),
            composer_defaults: ComposerDefaults::parse(zod::field(object, "composerDefaults")),
            disabled_providers,
            agent_defaults: AgentDefaults::parse(zod::field(object, "agentDefaults")),
            quota_routing: QuotaRouting::parse(zod::field(object, "quotaRouting")),
            projects,
            extra: object
                .map(|o| zod::extra_fields(o, WORKSPACE_CONFIG_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("schemaVersion", Value::from(self.schema_version)),
                ("projectsDir", Value::from(self.projects_dir.clone())),
                (
                    "modelsLocked",
                    self.models_locked.map(Value::from).unwrap_or(Value::Null),
                ),
                ("resources", self.resources.to_value()),
                ("composerDefaults", self.composer_defaults.to_value()),
                (
                    "disabledProviders",
                    Value::from(
                        self.disabled_providers
                            .iter()
                            .map(|p| runner_value(*p))
                            .collect::<Vec<_>>(),
                    ),
                ),
                ("agentDefaults", self.agent_defaults.to_value()),
                ("quotaRouting", self.quota_routing.to_value()),
                (
                    "projects",
                    Value::from(
                        self.projects
                            .iter()
                            .map(WorkspaceProject::to_value)
                            .collect::<Vec<_>>(),
                    ),
                ),
            ],
        )
    }
}

/// The last-known-good copy of a NON-EMPTY registry, written beside the config by every
/// successful merge-write.
pub fn workspace_config_backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

fn parse_workspace_config(raw: &str, env: &dyn EnvSource) -> Option<WorkspaceConfig> {
    let value: Value = serde_json::from_str(raw).ok()?;
    Some(WorkspaceConfig::parse(&value, env))
}

/// The snapshot is only worth restoring while it still holds projects — an empty one
/// carries no information the defaults do not already have.
fn load_workspace_config_backup(path: &Path, env: &dyn EnvSource) -> Option<WorkspaceConfig> {
    let raw = fs::read_to_string(workspace_config_backup_path(path)).ok()?;
    let parsed = parse_workspace_config(&raw, env)?;
    (!parsed.projects.is_empty()).then_some(parsed)
}

/// Read `~/.coducktor/config.json` on demand — never cached, never throws. Before
/// degrading, a missing/empty/corrupt file is restored from the `.bak` snapshot when
/// that still holds projects.
///
/// `path` defaults to the current `paths::workspace_config_path(env)`, but a caller that
/// will also WRITE should pass the path it resolved itself — see
/// `merge_write_workspace_config` for why resolving it twice is a data-loss bug.
pub fn load_workspace_config(path: &Path, env: &dyn EnvSource) -> WorkspaceConfig {
    let raw = fs::read_to_string(path).ok();
    if let Some(raw) = &raw
        && !raw.trim().is_empty()
        && let Some(parsed) = parse_workspace_config(raw, env)
    {
        return parsed;
    }
    if let Some(restored) = load_workspace_config_backup(path, env) {
        return restored;
    }
    WorkspaceConfig::default_for(env)
}

/// The tmp path an atomic write stages through — UNIQUE PER WRITE, never a fixed
/// `${path}.tmp`. The `~/.coducktor/` directory is shared
/// by every coducktor process on the machine, so two writers staging through the same tmp
/// name would interleave (writer B's truncate can empty the file between writer A's write
/// and rename). The pid + a random suffix gives every writer its own staging file.
pub fn atomic_tmp_path(path: &Path) -> PathBuf {
    let random = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!(
            "{:08x}",
            (nanos as u64) ^ (process::id() as u64).wrapping_mul(0x9E37_79B9)
        )
    };
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{random}.tmp", process::id()));
    path.with_file_name(name)
}

/// Atomic JSON write (`0600`, dir `0700`) via a per-writer tmp + rename — shared by every
/// writer in `workspace::*`.
pub fn atomic_write_json_sync(path: &Path, value: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        set_mode(dir, 0o700)?;
    }
    let tmp = atomic_tmp_path(path);
    let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, format!("{json}\n"))?;
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, path)?;
    let _ = set_mode(path, 0o600); // best-effort — ignored on some filesystems
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Read-modify-write merge: re-read the file, apply `mutator`, and atomically rename the write,
/// including its refreshed
/// `.bak` snapshot after every successful write (best-effort — a failed snapshot must
/// never turn a successful write into an error).
///
/// The path is resolved ONCE by the caller and passed in here, for the exact reason the
/// `paths::workspace_config_path` re-reads `DUCK_HOME` on every call, so resolving it twice (once
/// for the read, again for the
/// write) can send the two halves to different files if the environment changes between
/// them.
pub fn merge_write_workspace_config(
    path: &Path,
    env: &dyn EnvSource,
    mutator: impl FnOnce(&mut WorkspaceConfig),
) -> io::Result<WorkspaceConfig> {
    let mut next = load_workspace_config(path, env);
    mutator(&mut next);
    atomic_write_json_sync(path, &next.to_value())?;
    let _ = atomic_write_json_sync(&workspace_config_backup_path(path), &next.to_value());
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::FixedEnv;

    fn env() -> FixedEnv {
        FixedEnv::default()
    }

    #[test]
    fn defaults_match_the_shipped_node_output() {
        let config = WorkspaceConfig::default_for(&env());
        assert_eq!(config.schema_version, 0);
        assert_eq!(config.projects_dir, "~/coducktor/projects");
        assert_eq!(config.resources.max_parallel, 2);
        assert_eq!(config.resources.monitoring_wake_interval_minutes, Some(5));
        assert_eq!(config.resources.memory_limit_mb, None);
        assert_eq!(
            config.quota_routing.claude.long_window_stop_at_percent,
            95.0
        );
        assert_eq!(config.quota_routing.codex.long_window_stop_at_percent, 90.0);
        assert_eq!(
            config.quota_routing.provider_order,
            vec![
                QuotaProvider::Claude,
                QuotaProvider::Codex,
                QuotaProvider::OpenCode,
            ]
        );
        assert!(config.projects.is_empty());
        assert!(config.disabled_providers.is_empty());
    }

    #[test]
    fn zero_is_meaningful_for_worktree_retention_default() {
        let raw = serde_json::json!({ "resources": { "worktreeRetentionDefault": 0 } });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.resources.worktree_retention_default, 0);
    }

    #[test]
    fn an_explicit_null_wake_interval_is_preserved_not_replaced_by_the_default() {
        let raw = serde_json::json!({ "resources": { "monitoringWakeIntervalMinutes": null } });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.resources.monitoring_wake_interval_minutes, None);
    }

    #[test]
    fn a_bad_value_degrades_only_that_key() {
        let raw =
            serde_json::json!({ "resources": { "maxParallel": 4, "maxMonitoringSessions": 999 } });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.resources.max_parallel, 4, "the sibling key survives");
        assert_eq!(
            config.resources.max_monitoring_sessions, 2,
            "the bad key degrades to its default"
        );
    }

    #[test]
    fn a_bad_entry_is_dropped_without_evicting_the_registry() {
        let raw = serde_json::json!({
            "projects": [
                { "id": "shop", "root": "/repo/shop" },
                { "id": "NOT-A-VALID-ID!", "root": "/repo/bad" },
                { "id": "site", "root": "relative/not-absolute" },
            ],
        });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.projects.len(), 1);
        assert_eq!(config.projects[0].id, "shop");
    }

    #[test]
    fn disabled_providers_are_deduped_and_reordered_to_the_canonical_order() {
        let raw =
            serde_json::json!({ "disabledProviders": ["pi", "claude", "pi", "not-a-provider"] });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.disabled_providers, vec![Runner::Claude, Runner::Pi]);
    }

    #[test]
    fn unknown_top_level_and_nested_keys_round_trip() {
        let raw = serde_json::json!({
            "fromTheFuture": "keep me",
            "resources": { "alsoFromTheFuture": 42, "maxParallel": 3 },
        });
        let config = WorkspaceConfig::parse(&raw, &env());
        let written = config.to_value();
        assert_eq!(written["fromTheFuture"], "keep me");
        assert_eq!(written["resources"]["alsoFromTheFuture"], 42);
        assert_eq!(written["resources"]["maxParallel"], 3);
    }

    #[test]
    fn round_trip_through_parse_and_serialize_is_stable() {
        let raw = serde_json::json!({
            "schemaVersion": 2,
            "resources": { "maxParallel": 5 },
            "agentDefaults": { "runner": "codex", "models": { "codex": "gpt" } },
            "projects": [{ "id": "shop", "root": "/repo/shop", "tags": ["storefront"] }],
        });
        let once = WorkspaceConfig::parse(&raw, &env());
        let twice = WorkspaceConfig::parse(&once.to_value(), &env());
        assert_eq!(once, twice);
    }

    #[test]
    fn quota_routing_parses_opencode_policy_accounts_and_routes() {
        let raw = serde_json::json!({
            "quotaRouting": {
                "enabled": true,
                "qualityPreference": "best",
                "unknownUsagePolicy": "allow_with_penalty",
                "maxAutoAttemptsPerGeneration": 5,
                "providerOrder": ["opencode", "codex", "opencode"],
                "providers": {
                    "opencode": {
                        "enabled": true,
                        "priority": 80,
                        "maxConcurrentPerAccount": 2,
                        "futureProviderKey": true
                    }
                },
                "accounts": {
                    "work-claude": {
                        "autoEligible": false,
                        "priority": 80,
                        "futureAccountKey": "keep"
                    }
                },
                "routes": {
                    "opencode:default:anthropic/sonnet": {
                        "autoEligible": true,
                        "priority": 90
                    }
                }
            }
        });
        let config = WorkspaceConfig::parse(&raw, &env());
        // The retired global enable flag remains an unknown key so existing config files round
        // trip without making routing global again.
        assert_eq!(config.quota_routing.to_value()["enabled"], true);
        assert_eq!(
            config.quota_routing.provider_order,
            vec![QuotaProvider::OpenCode, QuotaProvider::Codex]
        );
        assert_eq!(
            config.quota_routing.quality_preference,
            QualityPreference::Best
        );
        assert_eq!(config.quota_routing.max_auto_attempts_per_generation, 5);
        assert_eq!(config.quota_routing.opencode.priority, 80);
        assert_eq!(config.quota_routing.opencode.max_concurrent_per_account, 2);
        assert!(!config.quota_routing.accounts["work-claude"].auto_eligible);
        assert!(config.quota_routing.routes["opencode:default:anthropic/sonnet"].auto_eligible);

        let written = config.to_value();
        assert_eq!(
            written["quotaRouting"]["providers"]["opencode"]["futureProviderKey"],
            true
        );
        assert_eq!(
            written["quotaRouting"]["accounts"]["work-claude"]["futureAccountKey"],
            "keep"
        );
    }

    #[test]
    fn legacy_max_concurrent_quota_key_round_trips_without_wire_drift() {
        let raw = serde_json::json!({
            "quotaRouting": {
                "providers": {"claude": {"maxConcurrent": 3}}
            }
        });
        let config = WorkspaceConfig::parse(&raw, &env());
        assert_eq!(config.quota_routing.claude.max_concurrent_per_account, 3);
        let written = config.to_value();
        assert_eq!(
            written["quotaRouting"]["providers"]["claude"]["maxConcurrent"],
            3
        );
        assert!(
            written["quotaRouting"]["providers"]["claude"]
                .get("maxConcurrentPerAccount")
                .is_none()
        );
    }

    #[test]
    fn merge_write_reads_the_file_it_just_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        merge_write_workspace_config(&path, &env(), |config| {
            config.resources.max_parallel = 7;
        })
        .unwrap();
        let reloaded = load_workspace_config(&path, &env());
        assert_eq!(reloaded.resources.max_parallel, 7);
        assert!(workspace_config_backup_path(&path).exists());
    }

    #[test]
    fn a_missing_config_restores_from_a_non_empty_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        merge_write_workspace_config(&path, &env(), |config| {
            config.projects.push(WorkspaceProject {
                id: "shop".to_owned(),
                root: "/repo/shop".to_owned(),
                name: String::new(),
                added_at: String::new(),
                last_opened_at: String::new(),
                source: ProjectSource::Local,
                max_parallel: None,
                tags: None,
                extra: Map::new(),
            });
        })
        .unwrap();
        fs::remove_file(&path).unwrap();
        let reloaded = load_workspace_config(&path, &env());
        assert_eq!(reloaded.projects.len(), 1);
    }

    #[test]
    fn an_empty_registry_is_never_restored_from_a_stale_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        merge_write_workspace_config(&path, &env(), |config| {
            config.projects.push(WorkspaceProject {
                id: "shop".to_owned(),
                root: "/repo/shop".to_owned(),
                name: String::new(),
                added_at: String::new(),
                last_opened_at: String::new(),
                source: ProjectSource::Local,
                max_parallel: None,
                tags: None,
                extra: Map::new(),
            });
        })
        .unwrap();
        // The user deliberately empties the registry — the refreshed `.bak` must not
        // resurrect the project on the next load.
        merge_write_workspace_config(&path, &env(), |config| {
            config.projects.clear();
        })
        .unwrap();
        let reloaded = load_workspace_config(&path, &env());
        assert!(reloaded.projects.is_empty());
    }
}
