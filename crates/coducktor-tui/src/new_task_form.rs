//! The new-task composer's picker rules and its POST body — a faithful Rust port
//! of `packages/web/src/routes/new-task-form.ts` and `new-task-draft.ts`. Every
//! rule lives here as a pure function so drift from the web client is a diff in
//! ONE file, not a scavenger hunt.
//!
//! Draft persistence (per-project, survives navigation for the lifetime of the
//! cockpit) lives on `App::new_task_drafts` — see `screens::new_task::sync_draft`.

use coducktor_contract::{
    ConfigResponse, CreateRunInput, CreateRunInputBase, CreateRunResponse, ImageInput,
    ProviderConnectionState, ProviderStatusResponse, ReasoningEffort, Runner,
    RunnerModelCatalogResponse, RunnerModels, RunnerSelection, Skill, TaskSource, WorkflowDef,
    WorkflowStepDef, runner_discovers_models,
};

/// What the composer runs: the plain-task baseline, a named workflow, or a single skill.
pub const BASELINE_SOURCE: TaskSource = TaskSource::Baseline;

fn same_source(a: &TaskSource, b: &TaskSource) -> bool {
    match (a, b) {
        (TaskSource::Baseline, TaskSource::Baseline) => true,
        (TaskSource::Skill { reference: a }, TaskSource::Skill { reference: b })
        | (TaskSource::Workflow { reference: a }, TaskSource::Workflow { reference: b }) => a == b,
        _ => false,
    }
}

/// Prepend `source` to the recency list (newest first), dropping any earlier
/// occurrence of the same source+ref, and cap the length.
pub fn push_recent_source(
    recent: Option<&[TaskSource]>,
    source: TaskSource,
    cap: usize,
) -> Vec<TaskSource> {
    let rest = recent
        .unwrap_or(&[])
        .iter()
        .filter(|existing| !same_source(existing, &source))
        .cloned()
        .collect::<Vec<_>>();
    let mut next = Vec::with_capacity(cap);
    next.push(source);
    next.extend(rest.into_iter().take(cap.saturating_sub(1)));
    next
}

/// The agent-backend catalog in `RUNNERS` order (legacy `RUNNERS`).
pub const RUNNERS: [Runner; 4] = [Runner::Claude, Runner::Codex, Runner::OpenCode, Runner::Pi];

/// Runners whose provider is both connected and enabled, in `RUNNERS` order —
/// the port of `lib/provider-status.ts::usableRunners`.
pub fn usable_runners(status: Option<&ProviderStatusResponse>) -> Vec<Runner> {
    let Some(status) = status else {
        return Vec::new();
    };
    let usable: Vec<Runner> = status
        .providers
        .iter()
        .filter(|row| row.enabled == Some(true) && row.status == ProviderConnectionState::Connected)
        .map(|row| row.provider)
        .collect();
    RUNNERS
        .iter()
        .copied()
        .filter(|runner| usable.contains(runner))
        .collect()
}

/// A model option in a runner's picker — preset, host-discovered, or a pinned
/// custom/native id.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPreset {
    pub id: String,
    pub label: String,
    pub desc: String,
    pub reasoning_efforts: Option<Vec<String>>,
}

/// The static presets per runner (legacy `MODELS_BY_RUNNER`). `id: ""` is always
/// "auto" — no model flag, the runner decides.
pub fn static_models_for(runner: Runner) -> Vec<ModelPreset> {
    let preset = |id: &'static str, label: &'static str, desc: &'static str| ModelPreset {
        id: id.to_owned(),
        label: label.to_owned(),
        desc: desc.to_owned(),
        reasoning_efforts: None,
    };
    match runner {
        Runner::Claude => vec![
            preset("", "auto", "Pick the best model per step"),
            preset("opus", "opus", "Deep reasoning for hard tasks"),
            preset("sonnet", "sonnet", "Fast and cheap"),
            preset("haiku", "haiku", "Fastest — simple, scoped tasks"),
            preset(
                "claude-fable-5",
                "Fable 5",
                "Most capable — the Claude 5 family",
            ),
            preset("claude-opus-4-8", "Opus 4.8", "Pinned version"),
            preset("claude-sonnet-5", "Sonnet 5", "Pinned version"),
            preset("claude-haiku-4-5", "Haiku 4.5", "Pinned version"),
        ],
        Runner::Codex | Runner::OpenCode => vec![preset("", "auto", "Use your default model")],
        Runner::Pi => vec![
            preset("", "auto", "Use your pi default model"),
            preset(
                "anthropic/claude-opus-4-8",
                "claude-opus-4.8",
                "via Anthropic",
            ),
            preset(
                "anthropic/claude-sonnet-5",
                "claude-sonnet-5",
                "via Anthropic",
            ),
            preset("openai/gpt-5.1", "gpt-5.1", "via OpenAI"),
        ],
    }
}

/// Runners that pick with the canonical `provider/model` convention and span every
/// provider the host has configured, so an id they list is never EXCLUSIVE to them.
const PROVIDER_SPANNING_RUNNERS: [Runner; 2] = [Runner::OpenCode, Runner::Pi];

/// Keep recognized presets from another backend out of a runner's custom-model
/// escape hatch (#480). Unknown ids remain valid custom models; only a known
/// cross-runner mismatch is discarded.
pub fn model_conflicts_with_runner(model: &str, runner: Runner) -> bool {
    if model.is_empty()
        || static_models_for(runner)
            .iter()
            .any(|preset| preset.id == model)
    {
        return false;
    }
    RUNNERS.iter().any(|other| {
        *other != runner
            && !PROVIDER_SPANNING_RUNNERS.contains(other)
            && static_models_for(*other)
                .iter()
                .any(|preset| !preset.id.is_empty() && preset.id == model)
    })
}

/// Build a runner's model list: static presets, then host-discovered models for the
/// discovery runners, then custom/native ids that are still representable.
pub fn models_for_runner(
    runner: Runner,
    catalog: Option<&RunnerModelCatalogResponse>,
    custom_ids: &[Option<&str>],
) -> Vec<ModelPreset> {
    let mut base = static_models_for(runner);
    let mut seen: std::collections::HashSet<String> =
        base.iter().map(|model| model.id.clone()).collect();
    if runner_discovers_models(runner) {
        for model in catalog.map(|catalog| &catalog.models).into_iter().flatten() {
            if model.id.is_empty() || seen.contains(&model.id) {
                continue;
            }
            seen.insert(model.id.clone());
            base.push(ModelPreset {
                id: model.id.clone(),
                label: if model.label.is_empty() {
                    model.id.clone()
                } else {
                    model.label.clone()
                },
                desc: model.description.clone(),
                reasoning_efforts: model.reasoning_efforts.clone(),
            });
        }
    }
    for id in custom_ids.iter().flatten().filter(|id| !id.is_empty()) {
        if seen.contains(*id) || model_conflicts_with_runner(id, runner) {
            continue;
        }
        seen.insert((*id).to_owned());
        base.push(ModelPreset {
            id: (*id).to_owned(),
            label: (*id).to_owned(),
            desc: "Custom or legacy model".to_owned(),
            reasoning_efforts: None,
        });
    }
    base
}

/// The effective runner: the user's pick when still installed, else the configured
/// default when installed, else the first available (legacy preselection order).
pub fn resolve_runner(picked: Option<Runner>, available: &[Runner], preferred: Runner) -> Runner {
    if let Some(picked) = picked
        && available.contains(&picked)
    {
        return picked;
    }
    if available.contains(&preferred) {
        return preferred;
    }
    available.first().copied().unwrap_or(Runner::Claude)
}

/// `auto` is an authored policy, not a constructible backend. Preserve it for the
/// composer; concrete picker consumers keep using `resolve_runner`.
pub fn resolve_runner_selection(
    picked: Option<RunnerSelection>,
    available: &[Runner],
    preferred: RunnerSelection,
) -> RunnerSelection {
    if picked == Some(RunnerSelection::Auto) {
        return RunnerSelection::Auto;
    }
    if picked.is_none()
        && preferred == RunnerSelection::Auto
        && available.contains(&Runner::Claude)
        && available.contains(&Runner::Codex)
    {
        return RunnerSelection::Auto;
    }
    let preferred_runner = selection_to_runner(preferred);
    let resolved = resolve_runner(picked.map(selection_to_runner), available, preferred_runner);
    runner_to_selection(resolved)
}

fn selection_to_runner(selection: RunnerSelection) -> Runner {
    match selection {
        RunnerSelection::Auto => Runner::Claude,
        RunnerSelection::Claude => Runner::Claude,
        RunnerSelection::Codex => Runner::Codex,
        RunnerSelection::OpenCode => Runner::OpenCode,
        RunnerSelection::Pi => Runner::Pi,
    }
}

fn runner_to_selection(runner: Runner) -> RunnerSelection {
    match runner {
        Runner::Claude => RunnerSelection::Claude,
        Runner::Codex => RunnerSelection::Codex,
        Runner::OpenCode => RunnerSelection::OpenCode,
        Runner::Pi => RunnerSelection::Pi,
    }
}

/// The runner field shared by every NEW-run surface. Explicit/sticky intent always
/// rides the request; only an untouched pick matching the active project's known
/// default may be omitted.
pub fn runner_override(
    runner: RunnerSelection,
    default_runner: Option<RunnerSelection>,
    explicit: bool,
) -> Option<RunnerSelection> {
    if !explicit && Some(runner) == default_runner {
        None
    } else {
        Some(runner)
    }
}

/// The effective model: the user's pick when it exists in the selected runner's
/// presets, else the configured per-runner default when IT is a known preset, else
/// auto (`""`). An explicit pick — including picking auto — always beats the
/// configured default.
pub fn resolve_model(
    picked: Option<&str>,
    runner: Runner,
    defaults: Option<&RunnerModels>,
    catalog: Option<&RunnerModelCatalogResponse>,
) -> String {
    let custom = [
        picked,
        defaults
            .and_then(|defaults| default_for_runner(defaults, runner))
            .map(|value| value.as_str()),
    ];
    let models = models_for_runner(runner, catalog, &custom);
    if let Some(picked) = picked
        && models.iter().any(|model| model.id == picked)
    {
        return picked.to_owned();
    }
    if let Some(preset) = defaults.and_then(|defaults| default_for_runner(defaults, runner))
        && models.iter().any(|model| model.id == *preset)
    {
        return preset.to_owned();
    }
    String::new()
}

fn default_for_runner(defaults: &RunnerModels, runner: Runner) -> Option<&String> {
    match runner {
        Runner::Claude => defaults.claude.as_ref(),
        Runner::Codex => defaults.codex.as_ref(),
        Runner::OpenCode => defaults.opencode.as_ref(),
        Runner::Pi => defaults.pi.as_ref(),
    }
}

/// One reasoning-level option in the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningOption {
    pub value: ReasoningEffort,
    pub label: &'static str,
    pub desc: &'static str,
}

pub fn reasoning_levels() -> Vec<ReasoningOption> {
    vec![
        ReasoningOption {
            value: ReasoningEffort::Auto,
            label: "Auto",
            desc: "Choose per work chunk",
        },
        ReasoningOption {
            value: ReasoningEffort::Low,
            label: "Low",
            desc: "Fast, focused execution",
        },
        ReasoningOption {
            value: ReasoningEffort::Medium,
            label: "Medium",
            desc: "Balanced reasoning",
        },
        ReasoningOption {
            value: ReasoningEffort::High,
            label: "High",
            desc: "Careful reasoning for harder work",
        },
        ReasoningOption {
            value: ReasoningEffort::XHigh,
            label: "Max",
            desc: "Deepest available reasoning",
        },
    ]
}

/// The available reasoning choices for the currently selected model. Catalogs may
/// advertise a narrower native set; Auto remains available.
pub fn reasoning_options_for_model(model: &str, models: &[ModelPreset]) -> Vec<ReasoningOption> {
    let advertised = models
        .iter()
        .find(|entry| entry.id == model)
        .and_then(|entry| entry.reasoning_efforts.as_ref());
    let Some(advertised) = advertised else {
        return reasoning_levels();
    };
    let supported: Vec<&str> = advertised.iter().map(String::as_str).collect();
    reasoning_levels()
        .into_iter()
        .filter(|option| {
            option.value == ReasoningEffort::Auto
                || supported.contains(&reasoning_effort_id(option.value))
        })
        .collect()
}

fn reasoning_effort_id(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Auto => "auto",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "xhigh",
    }
}

/// Whether a source still exists in the catalogs.
pub fn source_exists(source: &TaskSource, skills: &[Skill], workflows: &[WorkflowDef]) -> bool {
    match source {
        TaskSource::Baseline => true,
        TaskSource::Skill { reference } => skills.iter().any(|skill| skill.name == *reference),
        TaskSource::Workflow { reference } => {
            workflows.iter().any(|workflow| workflow.name == *reference)
        }
    }
}

/// The effective source: the first candidate that still exists, else the zero-config
/// cold default: the plain-task baseline.
pub fn resolve_source(
    candidates: &[Option<TaskSource>],
    skills: &[Skill],
    workflows: &[WorkflowDef],
) -> TaskSource {
    for candidate in candidates {
        if let Some(candidate) = candidate
            && source_exists(candidate, skills, workflows)
        {
            return candidate.clone();
        }
    }
    TaskSource::Baseline
}

/// The assembled `POST /runs` body options, mirroring `buildCreateRunBody`'s opts.
#[derive(Debug, Clone)]
pub struct CreateRunBodyOpts {
    pub task: String,
    pub source: TaskSource,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub models_locked: bool,
    pub runner: RunnerSelection,
    pub runner_explicit: bool,
    pub default_runner: Option<RunnerSelection>,
    pub agent_profile: Option<String>,
    pub variants: u64,
    pub images: Vec<ImageInput>,
    /// Resolved worktree flag: `false` → run in the repo working tree (single runs only).
    pub worktree: Option<bool>,
    pub autonomous: bool,
    pub generate_followups: bool,
}

fn task_step(id: &str, name: &str, skill: Option<&str>) -> WorkflowStepDef {
    WorkflowStepDef {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        prompt: Some("{{task}}".to_owned()),
        skill: skill.map(ToOwned::to_owned),
        model: None,
        runner: None,
        allowed_tools: None,
        bash_allowlist: None,
        command: None,
        on_fail: None,
    }
}

/// The exact `POST /api/v1/runs` body the composer sends — the port of
/// `buildCreateRunBody`: a skill runs as a one-step inline chain; a workflow goes by
/// name; `model`/`variants`/`images` only when they say something.
pub fn build_create_run_body(opts: &CreateRunBodyOpts) -> CreateRunInput {
    let (workflow, steps) = match &opts.source {
        TaskSource::Baseline => (None, Some(vec![task_step("task", "Baseline", None)])),
        TaskSource::Skill { reference } => (
            None,
            Some(vec![task_step("task", reference, Some(reference))]),
        ),
        TaskSource::Workflow { reference } => (Some(reference.clone()), None),
    };
    CreateRunInputBase {
        workflow,
        steps,
        task: opts.task.clone(),
        model: if opts.models_locked {
            None
        } else {
            (!opts.model.is_empty()).then(|| opts.model.clone())
        },
        reasoning_effort: opts
            .reasoning_effort
            .filter(|effort| *effort != ReasoningEffort::Auto),
        runner: runner_override(opts.runner, opts.default_runner, opts.runner_explicit),
        agent_profile: (!opts.agent_profile.as_deref().unwrap_or("").is_empty())
            .then(|| opts.agent_profile.clone())
            .flatten(),
        variants: (opts.variants > 1).then_some(opts.variants as f64),
        worktree: match opts.worktree {
            Some(false) if opts.variants <= 1 => Some(false),
            _ => None,
        },
        autonomous: opts.autonomous.then_some(true),
        generate_followups: (!opts.generate_followups).then_some(false),
        system_prompt: None,
        images: if opts.images.is_empty() {
            None
        } else {
            Some(opts.images.clone())
        },
        todo_id: None,
    }
}

/// Where a successful POST navigates: the run's thread — for ×2/×3 the FIRST
/// variant's thread, exactly what legacy `handleStarted` selects.
pub fn started_run_id(response: &CreateRunResponse) -> Option<String> {
    match response {
        CreateRunResponse::Single(record) => Some(record.id.clone()),
        CreateRunResponse::Group { runs } => runs.first().map(|record| record.id.clone()),
    }
}

/// Resolve run-mode values once, in precedence order: hard constraints, explicit
/// draft choices, an interactive-skill worktree recommendation, then configured
/// defaults (the port of `resolveComposerRunMode`).
pub fn resolve_composer_run_mode(
    has_git: bool,
    variants: u64,
    explicit_worktree: Option<bool>,
    interactive: bool,
    configured_autonomous: Option<bool>,
    configured_worktree: bool,
) -> (bool, bool) {
    let autonomous = configured_autonomous.unwrap_or(true);
    let worktree = if !has_git {
        false
    } else if variants > 1 {
        true
    } else {
        explicit_worktree
            .or_else(|| (interactive).then_some(false))
            .unwrap_or(configured_worktree)
    };
    (autonomous, worktree)
}

/// The `/new` header's one-line answer to "where will this run land?" (#793).
pub fn composer_run_mode_note(worktree: bool, has_git: bool) -> &'static str {
    if worktree {
        "Runs in an isolated worktree — review everything before it lands."
    } else if has_git {
        "Runs in the repo working tree — your checkout is modified directly."
    } else {
        "Runs in place — no git repository detected, so there is no worktree to isolate in."
    }
}

/// The config a project contributes to the composer's effective values.
#[derive(Debug, Clone)]
pub struct ComposerConfig {
    pub base_branch: Option<String>,
    pub default_runner: RunnerSelection,
    pub default_models: RunnerModels,
    pub models_locked: bool,
}

impl Default for ComposerConfig {
    fn default() -> Self {
        Self {
            base_branch: None,
            default_runner: RunnerSelection::Claude,
            default_models: RunnerModels::default(),
            models_locked: false,
        }
    }
}

impl ComposerConfig {
    pub fn from_config(config: &ConfigResponse) -> Self {
        Self {
            base_branch: config.base_branch.clone(),
            default_runner: config.default_runner,
            default_models: config.default_models.clone(),
            models_locked: config.models_locked,
        }
    }
}

/// The per-project new-task draft (port of `NewTaskDraft`). `None` fields mean
/// "the user has not chosen" — the form falls back to persisted/last-used/defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTaskDraft {
    pub text: String,
    pub source: Option<TaskSource>,
    pub runner: Option<RunnerSelection>,
    pub agent_profile: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub variants: u64,
    pub worktree: Option<bool>,
    /// TUI-only override: pin autonomous on/off; `None` follows the workspace default.
    pub autonomous: Option<bool>,
    pub generate_followups: Option<bool>,
}

impl Default for NewTaskDraft {
    fn default() -> Self {
        Self {
            text: String::new(),
            source: None,
            runner: None,
            agent_profile: None,
            model: None,
            reasoning_effort: None,
            variants: 1,
            worktree: None,
            autonomous: None,
            generate_followups: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, source: coducktor_contract::SkillSource) -> Skill {
        Skill {
            name: name.to_owned(),
            description: None,
            interactive: None,
            body: String::new(),
            path: format!("/skills/{name}.md"),
            source,
            team: None,
        }
    }

    fn workflow(name: &str) -> WorkflowDef {
        WorkflowDef {
            name: name.to_owned(),
            description: None,
            steps: Vec::new(),
            source: coducktor_contract::WorkflowSource::BuiltIn,
            path: None,
        }
    }

    fn source(kind: &str, ref_: &str) -> TaskSource {
        match kind {
            "skill" => TaskSource::Skill {
                reference: ref_.to_owned(),
            },
            "workflow" => TaskSource::Workflow {
                reference: ref_.to_owned(),
            },
            _ => TaskSource::Baseline,
        }
    }

    #[test]
    fn usable_runners_reads_the_provider_status() {
        let status = ProviderStatusResponse {
            providers: vec![
                coducktor_contract::ProviderStatus {
                    provider: Runner::Claude,
                    status: ProviderConnectionState::Connected,
                    enabled: Some(true),
                    hint: None,
                    auth_failure_id: None,
                    profile_id: None,
                },
                coducktor_contract::ProviderStatus {
                    provider: Runner::Codex,
                    status: ProviderConnectionState::Connected,
                    enabled: Some(false),
                    hint: None,
                    auth_failure_id: None,
                    profile_id: None,
                },
                coducktor_contract::ProviderStatus {
                    provider: Runner::OpenCode,
                    status: ProviderConnectionState::Disconnected,
                    enabled: Some(true),
                    hint: None,
                    auth_failure_id: None,
                    profile_id: None,
                },
            ],
        };
        assert_eq!(usable_runners(Some(&status)), vec![Runner::Claude]);
        assert!(usable_runners(None).is_empty());
    }

    #[test]
    fn resolve_runner_keeps_the_pick_while_installed() {
        assert_eq!(
            resolve_runner(
                Some(Runner::Codex),
                &[Runner::Claude, Runner::Codex],
                Runner::Claude
            ),
            Runner::Codex
        );
        assert_eq!(
            resolve_runner(
                Some(Runner::OpenCode),
                &[Runner::Claude, Runner::Codex],
                Runner::Codex
            ),
            Runner::Codex
        );
        assert_eq!(
            resolve_runner(None, &[Runner::Codex, Runner::OpenCode], Runner::Claude),
            Runner::Codex
        );
    }

    #[test]
    fn resolve_runner_selection_preserves_an_untouched_auto_default() {
        assert_eq!(
            resolve_runner_selection(
                None,
                &[Runner::Claude, Runner::Codex],
                RunnerSelection::Auto
            ),
            RunnerSelection::Auto
        );
    }

    #[test]
    fn claude_model_catalog_matches_the_web_order() {
        let ids: Vec<String> = models_for_runner(Runner::Claude, None, &[])
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            [
                "",
                "opus",
                "sonnet",
                "haiku",
                "claude-fable-5",
                "claude-opus-4-8",
                "claude-sonnet-5",
                "claude-haiku-4-5"
            ]
        );
    }

    #[test]
    fn codex_models_combine_auto_discovery_and_custom_ids() {
        let catalog = RunnerModelCatalogResponse {
            runner: Runner::Codex,
            models: vec![coducktor_contract::RunnerModelOption {
                id: "gpt-future".to_owned(),
                label: "Future".to_owned(),
                description: "New".to_owned(),
                reasoning_efforts: None,
            }],
            source: coducktor_contract::ModelCatalogSource::Live,
            stale: false,
            reason: None,
        };
        let ids: Vec<String> =
            models_for_runner(Runner::Codex, Some(&catalog), &[Some("legacy-id")])
                .into_iter()
                .map(|model| model.id)
                .collect();
        assert_eq!(ids, ["", "gpt-future", "legacy-id"]);
    }

    #[test]
    fn reasoning_options_are_limited_to_the_advertised_set() {
        let models = vec![ModelPreset {
            id: "gpt-limited".to_owned(),
            label: "Limited".to_owned(),
            desc: String::new(),
            reasoning_efforts: Some(vec!["medium".to_owned()]),
        }];
        let options = reasoning_options_for_model("gpt-limited", &models);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![ReasoningEffort::Auto, ReasoningEffort::Medium]
        );
        assert_eq!(
            reasoning_options_for_model("", &models)
                .into_iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                ReasoningEffort::Auto,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ]
        );
    }

    #[test]
    fn a_provider_spanning_runners_preset_is_never_another_runners_exclusive_model() {
        for model in ["openai/gpt-5.1", "anthropic/claude-sonnet-5"] {
            assert!(!model_conflicts_with_runner(model, Runner::OpenCode));
            assert!(!model_conflicts_with_runner(model, Runner::Pi));
        }
        assert!(model_conflicts_with_runner("opus", Runner::Codex));
    }

    #[test]
    fn resolve_model_keeps_known_picks_and_falls_back_to_defaults() {
        assert_eq!(
            resolve_model(Some("opus"), Runner::Claude, None, None),
            "opus"
        );
        assert_eq!(
            resolve_model(Some("custom-codex-id"), Runner::Codex, None, None),
            "custom-codex-id"
        );
        assert_eq!(resolve_model(None, Runner::Claude, None, None), "");
        let defaults = RunnerModels {
            claude: Some("opus".to_owned()),
            ..RunnerModels::default()
        };
        assert_eq!(
            resolve_model(None, Runner::Claude, Some(&defaults), None),
            "opus"
        );
        assert_eq!(
            resolve_model(Some("sonnet"), Runner::Claude, Some(&defaults), None),
            "sonnet"
        );
        assert_eq!(
            resolve_model(Some(""), Runner::Claude, Some(&defaults), None),
            ""
        );
    }

    #[test]
    fn resolve_source_takes_the_first_existing_candidate_then_defaults_to_baseline() {
        let skills = vec![skill("om-fix", coducktor_contract::SkillSource::Ai)];
        let workflows = vec![workflow("quick-task")];
        assert_eq!(
            resolve_source(
                &[
                    Some(source("skill", "gone")),
                    Some(source("workflow", "quick-task"))
                ],
                &skills,
                &workflows,
            ),
            source("workflow", "quick-task")
        );
        assert_eq!(
            resolve_source(&[], &skills, &workflows),
            TaskSource::Baseline
        );
        assert_eq!(resolve_source(&[], &[], &[]), TaskSource::Baseline);
        assert!(!source_exists(
            &source("skill", "quick-task"),
            &skills,
            &workflows
        ));
        assert!(!source_exists(
            &source("workflow", "om-fix"),
            &skills,
            &workflows
        ));
        assert!(source_exists(&TaskSource::Baseline, &[], &[]));
    }

    fn base_opts() -> CreateRunBodyOpts {
        CreateRunBodyOpts {
            task: "do the thing".to_owned(),
            source: TaskSource::Baseline,
            model: String::new(),
            reasoning_effort: None,
            models_locked: false,
            runner: RunnerSelection::Claude,
            runner_explicit: false,
            default_runner: Some(RunnerSelection::Claude),
            agent_profile: None,
            variants: 1,
            images: Vec::new(),
            worktree: None,
            autonomous: false,
            generate_followups: true,
        }
    }

    #[test]
    fn baseline_source_builds_a_plain_one_step_chain() {
        let body = build_create_run_body(&base_opts());
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "task": "do the thing",
                "steps": [{ "id": "task", "name": "Baseline", "prompt": "{{task}}" }],
            })
        );
    }

    #[test]
    fn workflow_source_sends_workflow_by_name_and_omits_defaults() {
        let mut opts = base_opts();
        opts.source = source("workflow", "quick-task");
        let body = build_create_run_body(&opts);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "task": "do the thing", "workflow": "quick-task" })
        );
    }

    #[test]
    fn skill_source_builds_the_inline_skill_chain() {
        let mut opts = base_opts();
        opts.source = source("skill", "om-fix");
        opts.model = "sonnet".to_owned();
        opts.default_runner = Some(RunnerSelection::Codex);
        let body = build_create_run_body(&opts);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "task": "do the thing",
                "steps": [{ "id": "task", "name": "om-fix", "skill": "om-fix", "prompt": "{{task}}" }],
                "model": "sonnet",
                "runner": "claude",
            })
        );
    }

    #[test]
    fn explicit_reasoning_is_sent_but_auto_is_omitted() {
        let mut opts = base_opts();
        assert!(build_create_run_body(&opts).reasoning_effort.is_none());
        opts.reasoning_effort = Some(ReasoningEffort::High);
        assert_eq!(
            build_create_run_body(&opts).reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn runner_is_omitted_only_when_untouched_and_equal_to_the_default() {
        let mut opts = base_opts();
        opts.runner = RunnerSelection::Codex;
        opts.default_runner = Some(RunnerSelection::Codex);
        assert!(build_create_run_body(&opts).runner.is_none());
        opts.runner_explicit = true;
        assert_eq!(
            build_create_run_body(&opts).runner,
            Some(RunnerSelection::Codex)
        );
        opts.runner_explicit = false;
        opts.default_runner = Some(RunnerSelection::Claude);
        assert_eq!(
            build_create_run_body(&opts).runner,
            Some(RunnerSelection::Codex)
        );
    }

    #[test]
    fn worktree_false_is_sent_only_for_a_single_run() {
        let mut opts = base_opts();
        opts.worktree = Some(false);
        assert_eq!(build_create_run_body(&opts).worktree, Some(false));
        opts.worktree = Some(true);
        assert!(build_create_run_body(&opts).worktree.is_none());
        opts.worktree = Some(false);
        opts.variants = 2;
        assert!(build_create_run_body(&opts).worktree.is_none());
    }

    #[test]
    fn generate_followups_false_is_sent_only_when_disabled() {
        let mut opts = base_opts();
        opts.generate_followups = false;
        assert_eq!(build_create_run_body(&opts).generate_followups, Some(false));
        opts.generate_followups = true;
        assert!(build_create_run_body(&opts).generate_followups.is_none());
    }

    #[test]
    fn variants_and_images_ride_along() {
        let mut opts = base_opts();
        opts.source = source("workflow", "quick-task");
        opts.variants = 3;
        opts.images = vec![ImageInput {
            media_type: "image/png".to_owned(),
            data: "aGk=".to_owned(),
        }];
        let body = build_create_run_body(&opts);
        assert_eq!(body.variants, Some(3.0));
        assert_eq!(body.images, Some(opts.images.clone()));
    }

    #[test]
    fn started_run_id_selects_the_first_variant() {
        let single = CreateRunResponse::Single(Box::new(coducktor_contract::RunRecord {
            id: "r1".to_owned(),
            ..coducktor_contract::RunRecord::default()
        }));
        assert_eq!(started_run_id(&single).as_deref(), Some("r1"));
        let group = CreateRunResponse::Group {
            runs: vec![
                coducktor_contract::RunRecord {
                    id: "v-a".to_owned(),
                    ..Default::default()
                },
                coducktor_contract::RunRecord {
                    id: "v-b".to_owned(),
                    ..Default::default()
                },
            ],
        };
        assert_eq!(started_run_id(&group).as_deref(), Some("v-a"));
    }

    #[test]
    fn push_recent_source_dedups_same_source_and_caps() {
        let a = source("skill", "a");
        let b = source("skill", "b");
        let after = push_recent_source(Some(&[a.clone(), b.clone()]), b.clone(), 24);
        assert_eq!(after, vec![b.clone(), a.clone()]);
        let seed: Vec<TaskSource> = (0..24)
            .map(|index| source("skill", &format!("k{index}")))
            .collect();
        let after = push_recent_source(Some(&seed), source("skill", "new"), 24);
        assert_eq!(after.len(), 24);
        assert_eq!(after[0], source("skill", "new"));
        assert_eq!(after[23], source("skill", "k22"));
        assert_eq!(
            push_recent_source(None, source("skill", "a"), 24),
            vec![source("skill", "a")]
        );
    }

    #[test]
    fn resolve_composer_run_mode_follows_the_precedence_order() {
        assert_eq!(
            resolve_composer_run_mode(true, 1, None, false, None, true),
            (true, true)
        );
        assert_eq!(
            resolve_composer_run_mode(false, 1, None, false, None, true),
            (true, false)
        );
        assert_eq!(
            resolve_composer_run_mode(true, 3, None, false, None, true),
            (true, true)
        );
        assert_eq!(
            resolve_composer_run_mode(true, 1, Some(false), false, None, true),
            (true, false)
        );
        assert_eq!(
            resolve_composer_run_mode(true, 1, None, true, None, true),
            (true, false)
        );
        assert_eq!(
            resolve_composer_run_mode(true, 1, None, false, Some(false), true),
            (false, true)
        );
    }
}
