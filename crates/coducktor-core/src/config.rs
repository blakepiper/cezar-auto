//! `.ai/coducktor/config.json` — the optional per-repo config. Mirrors
//! `packages/coducktor/src/config.ts`. Zero-config rule: a missing file behaves exactly like
//! the default below; an unreadable or invalid file degrades to the default too (never
//! blocks startup).
//!
//! Unlike `workspace::config` (every field `.catch`'d, so one bad key never evicts its
//! siblings), seven fields here have a `.default()` with NO `.catch()` — `maxParallel`,
//! `defaultRunner`, `plannerModel`, `namerModel`, `liveTitleUpdates`, `reviewGate`,
//! `baseBranch`. Zod's `.default(x)` only fires when the key is `undefined`; a PRESENT but
//! invalid value on any of these fails the whole-object parse, and `loadConfig` then
//! discards the entire raw file rather than salvaging the other keys — "malformed
//! degrades to the full default, never a partial one" is a real, load-bearing rule here,
//! not just JSON-syntax-error handling. That's why this module validates as one
//! all-or-nothing pass (`try_parse`) instead of the field-by-field salvage
//! `workspace::config` and `workspace::agent_accounts` use.

use serde_json::Value;

use coducktor_contract::{Runner, RunnerModels, RunnerSelection};

use crate::workspace::config::{AgentDefaultModels, AgentDefaults as WorkspaceAgentDefaults};
use crate::zod;

/// Last-resort retention when neither the repo nor the workspace says anything. Mirrors
/// `config.ts::DEFAULT_WORKTREE_RETENTION`.
pub const DEFAULT_WORKTREE_RETENTION: u64 = 10;

/// Mirrors `config.ts::CezConfig` — the parsed shape of `.ai/coducktor/config.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoConfig {
    pub max_parallel: u64,
    pub worktree_retention: u64,
    pub memory_limit_mb: Option<u64>,
    pub default_runner: RunnerSelection,
    pub planner_model: String,
    pub namer_model: String,
    pub live_title_updates: Option<bool>,
    pub review_gate: Option<bool>,
    pub base_branch: Option<String>,
    pub system_prompt: Option<String>,
    pub default_models: RunnerModels,
    pub models_locked: Option<bool>,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            max_parallel: 2,
            worktree_retention: DEFAULT_WORKTREE_RETENTION,
            memory_limit_mb: None,
            default_runner: RunnerSelection::Claude,
            planner_model: "sonnet".to_owned(),
            namer_model: "haiku".to_owned(),
            live_title_updates: None,
            review_gate: None,
            base_branch: None,
            system_prompt: None,
            default_models: RunnerModels::default(),
            models_locked: None,
        }
    }
}

/// Auxiliary planner/namer calls are outside MVP quota routing and need a concrete
/// backend. Mirrors `config.ts::auxiliaryRunner`.
pub fn auxiliary_runner(selection: RunnerSelection) -> Runner {
    match selection {
        RunnerSelection::Auto => Runner::Claude,
        RunnerSelection::Claude => Runner::Claude,
        RunnerSelection::Codex => Runner::Codex,
        RunnerSelection::OpenCode => Runner::OpenCode,
        RunnerSelection::Pi => Runner::Pi,
    }
}

/// Fold the machine-wide agent defaults under a repo's own raw config, before validation.
/// Mirrors `config.ts::withMachineDefaults`: a repo key always wins, and `models` merges
/// PER RUNNER rather than wholesale, so pinning claude's model in one repo doesn't
/// silently discard the machine's codex preset.
fn with_machine_defaults(raw: &Value, machine: &WorkspaceAgentDefaults) -> Value {
    let mut own = raw.as_object().cloned().unwrap_or_default();

    if !own.contains_key("defaultRunner")
        && let Some(runner) = &machine.runner
    {
        own.insert(
            "defaultRunner".to_owned(),
            serde_json::to_value(runner).expect("RunnerSelection always serializes"),
        );
    }

    let own_models = own
        .get("defaultModels")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut models = machine
        .models
        .as_ref()
        .map(runner_models_to_map)
        .unwrap_or_default();
    for (key, value) in own_models {
        models.insert(key, value);
    }
    if !models.is_empty() {
        own.insert("defaultModels".to_owned(), Value::Object(models));
    }

    Value::Object(own)
}

fn runner_models_to_map(models: &AgentDefaultModels) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Some(v) = &models.claude {
        map.insert("claude".to_owned(), Value::String(v.clone()));
    }
    if let Some(v) = &models.codex {
        map.insert("codex".to_owned(), Value::String(v.clone()));
    }
    if let Some(v) = &models.opencode {
        map.insert("opencode".to_owned(), Value::String(v.clone()));
    }
    if let Some(v) = &models.pi {
        map.insert("pi".to_owned(), Value::String(v.clone()));
    }
    map
}

/// All-or-nothing validation against `configSchema`: `None` means at least one of the
/// seven non-`.catch`'d fields was present but invalid, so the caller must discard the
/// entire raw object (`config.ts`'s `configSchema.safeParse` failing).
fn try_parse(raw: &Value) -> Option<RepoConfig> {
    let object = raw.as_object()?;

    let max_parallel = strict_bounded_u64(object.get("maxParallel"), 1, 16, 2)?;
    let default_runner =
        strict_runner_selection(object.get("defaultRunner"), RunnerSelection::Claude)?;
    let planner_model = strict_min_len_str(object.get("plannerModel"), 1, "sonnet")?;
    let namer_model = strict_min_len_str(object.get("namerModel"), 1, "haiku")?;
    let live_title_updates = strict_bool_opt(object.get("liveTitleUpdates"))?;
    let review_gate = strict_bool_opt(object.get("reviewGate"))?;
    let base_branch = strict_trimmed_str_opt(object.get("baseBranch"), 1, usize::MAX)?;

    // The remaining five fields DO carry `.catch()` — a bad value degrades in place.
    let worktree_retention = zod::bounded_i64(
        object.get("worktreeRetention"),
        0,
        1000,
        DEFAULT_WORKTREE_RETENTION as i64,
    ) as u64;
    let memory_limit_mb = object
        .get("memoryLimitMb")
        .and_then(|v| zod::bounded_i64_nullable(Some(v), 0, 1_048_576, None))
        .map(|v| v as u64);
    let system_prompt = zod::trimmed_str_opt(object.get("systemPrompt"), 1, 20_000);
    let default_models = object
        .get("defaultModels")
        .and_then(Value::as_object)
        .map(|models| RunnerModels {
            claude: zod::trimmed_str_opt(models.get("claude"), 1, 200),
            codex: zod::trimmed_str_opt(models.get("codex"), 1, 200),
            opencode: zod::trimmed_str_opt(models.get("opencode"), 1, 200),
            pi: zod::trimmed_str_opt(models.get("pi"), 1, 200),
        })
        .unwrap_or_default();
    let models_locked = zod::bool_opt(object.get("modelsLocked"));

    Some(RepoConfig {
        max_parallel,
        worktree_retention,
        memory_limit_mb,
        default_runner,
        planner_model,
        namer_model,
        live_title_updates,
        review_gate,
        base_branch,
        system_prompt,
        default_models,
        models_locked,
    })
}

fn strict_bounded_u64(value: Option<&Value>, lo: i64, hi: i64, default: i64) -> Option<u64> {
    match value {
        None => Some(default as u64),
        Some(v) => {
            let n = v
                .as_i64()
                .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))?;
            (n >= lo && n <= hi).then_some(n as u64)
        }
    }
}

fn strict_runner_selection(
    value: Option<&Value>,
    default: RunnerSelection,
) -> Option<RunnerSelection> {
    match value {
        None => Some(default),
        Some(v) => serde_json::from_value(v.clone()).ok(),
    }
}

fn strict_min_len_str(value: Option<&Value>, lo: usize, default: &str) -> Option<String> {
    match value {
        None => Some(default.to_owned()),
        Some(Value::String(s)) => (s.chars().count() >= lo).then(|| s.clone()),
        Some(_) => None,
    }
}

fn strict_bool_opt(value: Option<&Value>) -> Option<Option<bool>> {
    match value {
        None => Some(None),
        Some(Value::Bool(b)) => Some(Some(*b)),
        Some(_) => None,
    }
}

fn strict_trimmed_str_opt(value: Option<&Value>, lo: usize, hi: usize) -> Option<Option<String>> {
    match value {
        None => Some(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            let len = trimmed.chars().count();
            (len >= lo && len <= hi).then(|| Some(trimmed.to_owned()))
        }
        Some(_) => None,
    }
}

/// The in-memory default with the machine's agent defaults folded in — what a missing or
/// unparseable `.ai/coducktor/config.json` behaves like.
fn default_with_machine(machine: &WorkspaceAgentDefaults) -> RepoConfig {
    try_parse(&with_machine_defaults(
        &Value::Object(Default::default()),
        machine,
    ))
    .expect("an all-default object always validates")
}

/// Read `.ai/coducktor/config.json` on demand — never cached, never throws. Also reads
/// `~/.coducktor/config.json`'s `agentDefaults`, deliberately not cached for the same
/// reason: it is shared by every coducktor process on the machine, so a snapshot is a
/// staleness bug. Mirrors `config.ts::loadConfig`.
pub fn load_config(repo_root: &std::path::Path, machine: &WorkspaceAgentDefaults) -> RepoConfig {
    let default = default_with_machine(machine);
    let raw = match std::fs::read_to_string(repo_root.join(".ai/coducktor/config.json")) {
        Ok(raw) => raw,
        Err(_) => return default,
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return default;
    };
    try_parse(&with_machine_defaults(&parsed, machine)).unwrap_or(default)
}

/// The repo's OWN `worktreeRetention`, or `None` when it doesn't set one. `load_config`
/// cannot answer this — `.catch(10)` materializes the key, so a parsed config can't tell
/// "the user chose 10" from "the user said nothing". Mirrors `config.ts::ownWorktreeRetention`
/// by probing the raw file instead.
fn own_worktree_retention(repo_root: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(repo_root.join(".ai/coducktor/config.json")).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let value = parsed.as_object()?.get("worktreeRetention")?;
    let n = value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0)
            .map(|f| f as i64)
    })?;
    (0..=1000).contains(&n).then_some(n as u64)
}

/// Effective worktree retention for a repo: the repo's own value wins whenever it sets
/// one; otherwise the workspace's `resources.worktreeRetentionDefault` seeds it; an
/// absent/unreadable workspace config keeps the historical 10. Mirrors
/// `config.ts::resolveWorktreeRetention`.
pub fn resolve_worktree_retention(
    repo_root: &std::path::Path,
    workspace_default: Option<u64>,
) -> u64 {
    own_worktree_retention(repo_root)
        .unwrap_or_else(|| workspace_default.unwrap_or(DEFAULT_WORKTREE_RETENTION))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(runner: Option<RunnerSelection>) -> WorkspaceAgentDefaults {
        WorkspaceAgentDefaults {
            runner,
            models: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let config = default_with_machine(&machine(None));
        assert_eq!(config, RepoConfig::default());
    }

    #[test]
    fn additive_keys_round_trip() {
        let raw = serde_json::json!({ "maxParallel": 4, "worktreeRetention": 0 });
        let parsed = try_parse(&with_machine_defaults(&raw, &machine(None))).unwrap();
        assert_eq!(parsed.max_parallel, 4);
        assert_eq!(
            parsed.worktree_retention, 0,
            "0 is meaningful (unlimited), not falsy-treated"
        );
    }

    #[test]
    fn an_invalid_non_catch_field_discards_the_whole_object() {
        let raw = serde_json::json!({ "maxParallel": 4, "defaultRunner": "not-a-runner" });
        assert!(try_parse(&raw).is_none());
    }

    #[test]
    fn an_invalid_catch_field_degrades_alone() {
        let raw = serde_json::json!({ "maxParallel": 4, "worktreeRetention": 99999 });
        let parsed = try_parse(&raw).unwrap();
        assert_eq!(parsed.max_parallel, 4);
        assert_eq!(parsed.worktree_retention, DEFAULT_WORKTREE_RETENTION);
    }

    #[test]
    fn machine_default_runner_is_folded_in_when_the_repo_is_silent() {
        let raw = serde_json::json!({});
        let parsed = try_parse(&with_machine_defaults(
            &raw,
            &machine(Some(RunnerSelection::Codex)),
        ))
        .unwrap();
        assert_eq!(parsed.default_runner, RunnerSelection::Codex);
    }

    #[test]
    fn a_repo_default_runner_always_wins_over_the_machine() {
        let raw = serde_json::json!({ "defaultRunner": "pi" });
        let parsed = try_parse(&with_machine_defaults(
            &raw,
            &machine(Some(RunnerSelection::Codex)),
        ))
        .unwrap();
        assert_eq!(parsed.default_runner, RunnerSelection::Pi);
    }

    #[test]
    fn default_models_merge_per_runner_not_wholesale() {
        let machine = WorkspaceAgentDefaults {
            runner: None,
            models: Some(AgentDefaultModels {
                claude: Some("machine-claude".to_owned()),
                codex: Some("machine-codex".to_owned()),
                opencode: None,
                pi: None,
                extra: Default::default(),
            }),
            extra: Default::default(),
        };
        let raw = serde_json::json!({ "defaultModels": { "claude": "repo-claude" } });
        let parsed = try_parse(&with_machine_defaults(&raw, &machine)).unwrap();
        assert_eq!(parsed.default_models.claude.as_deref(), Some("repo-claude"));
        assert_eq!(
            parsed.default_models.codex.as_deref(),
            Some("machine-codex")
        );
    }

    #[test]
    fn auxiliary_runner_maps_auto_to_claude() {
        assert_eq!(auxiliary_runner(RunnerSelection::Auto), Runner::Claude);
        assert_eq!(auxiliary_runner(RunnerSelection::Pi), Runner::Pi);
    }
}
