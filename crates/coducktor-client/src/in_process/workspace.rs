// ---- per-repo config helpers --------------------------------------------------------------
// parse_set_config_input/config_models_locked/read_repo_config functions -------------------

fn repo_config_path_at(repo_root: &Path, state_home: &Path) -> PathBuf {
    project_state_dir_in(state_home, repo_root).join("config.json")
}

fn repo_config_path(repo_root: &Path) -> PathBuf {
    data_dir(repo_root).join("config.json")
}

fn read_repo_config(repo_root: &Path, state_home: &Path) -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(repo_config_path_at(repo_root, state_home)) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn workspace_config_for(repo_root: &Path) -> coducktor_core::workspace::config::WorkspaceConfig {
    let _ = repo_root; // the workspace config is host-wide, not per-repo — kept for call-site symmetry
    load_workspace_config(
        &coducktor_core::paths::workspace_config_path(&ProcessEnv),
        &ProcessEnv,
    )
}

fn config_models_locked(repo_root: &Path, config: &coducktor_core::config::RepoConfig) -> bool {
    std::env::var("DUCK_AGENT_MODELS_LOCKED").is_ok_and(|value| value == "1")
        || workspace_config_for(repo_root).models_locked == Some(true)
        || config.models_locked == Some(true)
}

fn config_response(repo_root: &Path, state_home: &Path) -> ConfigResponse {
    let workspace = workspace_config_for(repo_root);
    let config = coducktor_core::config::load_config(
        &repo_config_path_at(repo_root, state_home),
        &workspace.agent_defaults,
    );
    let models_locked = config_models_locked(repo_root, &config);
    let composer_defaults = (!config.composer_defaults.reasoning.is_none()
        || !config.composer_defaults.variants.is_none()
        || !config.composer_defaults.autonomous.is_none()
        || !config.composer_defaults.worktree.is_none()
        || !config.composer_defaults.git_auto.is_none())
    .then_some(coducktor_contract::ProjectComposerDefaults {
        reasoning: config.composer_defaults.reasoning,
        variants: config.composer_defaults.variants,
        autonomous: config.composer_defaults.autonomous,
        worktree: config.composer_defaults.worktree,
        git_auto: config.composer_defaults.git_auto,
    });
    ConfigResponse {
        base_branch: config.base_branch,
        default_runner: config.default_runner,
        system_prompt: config.system_prompt,
        default_models: if models_locked {
            coducktor_contract::RunnerModels::default()
        } else {
            config.default_models
        },
        composer_defaults,
        models_locked,
        max_parallel: config.max_parallel,
        memory_limit_mb: config.memory_limit_mb,
        worktree_retention: config.worktree_retention,
        live_title_updates: config.live_title_updates,
        review_gate: config.review_gate,
    }
}

fn validate_set_config_input(input: &SetConfigInput) -> Result<(), EngineError> {
    if input
        .base_branch
        .as_ref()
        .and_then(|value| value.as_ref())
        .is_some_and(|value| {
            let trimmed = value.trim();
            trimmed.is_empty() || trimmed.chars().count() > 200
        })
    {
        return Err(EngineError::Conflict {
            reason: "baseBranch must be between 1 and 200 characters".to_owned(),
        });
    }
    if input
        .system_prompt
        .as_ref()
        .and_then(|value| value.as_ref())
        .is_some_and(|value| value.trim().chars().count() > 20_000)
    {
        return Err(EngineError::Conflict {
            reason: "systemPrompt must be at most 20000 characters".to_owned(),
        });
    }
    if input
        .max_parallel
        .is_some_and(|value| !(1..=16).contains(&value))
    {
        return Err(EngineError::Conflict {
            reason: "maxParallel must be an integer from 1 to 16".to_owned(),
        });
    }
    if input
        .memory_limit_mb
        .flatten()
        .is_some_and(|value| value > 1_048_576)
    {
        return Err(EngineError::Conflict {
            reason: "memoryLimitMb must be an integer from 0 to 1048576".to_owned(),
        });
    }
    if input
        .worktree_retention
        .flatten()
        .is_some_and(|value| value > 1000)
    {
        return Err(EngineError::Conflict {
            reason: "worktreeRetention must be an integer from 0 to 1000".to_owned(),
        });
    }
    if input
        .composer_defaults
        .as_ref()
        .and_then(|defaults| defaults.variants)
        .flatten()
        .is_some_and(|value| !(1..=3).contains(&value))
    {
        return Err(EngineError::Conflict {
            reason: "composer variants must be an integer from 1 to 3".to_owned(),
        });
    }
    Ok(())
}

fn apply_project_composer_defaults(
    raw: &mut serde_json::Map<String, Value>,
    patch: &coducktor_contract::ComposerDefaultsPatch,
) {
    let mut defaults = raw
        .get("composerDefaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if let Some(reasoning) = patch.reasoning {
        match reasoning {
            Some(value) => {
                defaults.insert(
                    "reasoning".to_owned(),
                    serde_json::to_value(value).unwrap_or(Value::Null),
                );
            }
            None => {
                defaults.remove("reasoning");
            }
        }
    }
    if let Some(variants) = patch.variants {
        match variants {
            Some(value) => {
                defaults.insert("variants".to_owned(), Value::from(value));
            }
            None => {
                defaults.remove("variants");
            }
        }
    }
    if let Some(autonomous) = patch.autonomous {
        match autonomous {
            Some(value) => {
                defaults.insert("autonomous".to_owned(), Value::Bool(value));
            }
            None => {
                defaults.remove("autonomous");
            }
        }
    }
    if let Some(worktree) = patch.worktree {
        match worktree {
            Some(value) => {
                defaults.insert("worktree".to_owned(), Value::Bool(value));
            }
            None => {
                defaults.remove("worktree");
            }
        }
    }
    if let Some(git_auto) = patch.git_auto {
        match git_auto {
            Some(value) => {
                defaults.insert("gitAuto".to_owned(), Value::Bool(value));
            }
            None => {
                defaults.remove("gitAuto");
            }
        }
    }

    if defaults.is_empty() {
        raw.remove("composerDefaults");
    } else {
        raw.insert("composerDefaults".to_owned(), Value::Object(defaults));
    }
}

fn update_repo_config(
    repo_root: &Path,
    state_home: &Path,
    input: &SetConfigInput,
) -> Result<ConfigResponse, EngineError> {
    validate_set_config_input(input)?;
    let workspace = workspace_config_for(repo_root);
    let current = coducktor_core::config::load_config(
        &repo_config_path_at(repo_root, state_home),
        &workspace.agent_defaults,
    );
    if config_models_locked(repo_root, &current) && input.default_models.is_some() {
        return Err(EngineError::Conflict {
            reason:
                "agent models are locked — configure the model in the native coding-agent settings"
                    .to_owned(),
        });
    }

    let mut raw = read_repo_config(repo_root, state_home);
    if let Some(base_branch) = &input.base_branch {
        match base_branch {
            None => {
                raw.remove("baseBranch");
            }
            Some(value) => {
                raw.insert(
                    "baseBranch".to_owned(),
                    Value::String(value.trim().to_owned()),
                );
            }
        }
    }
    if let Some(default_runner) = input.default_runner {
        raw.insert(
            "defaultRunner".to_owned(),
            serde_json::to_value(default_runner).unwrap_or(Value::Null),
        );
    }
    if let Some(system_prompt) = &input.system_prompt {
        match system_prompt.as_deref().map(str::trim) {
            None | Some("") => {
                raw.remove("systemPrompt");
            }
            Some(prompt) => {
                raw.insert("systemPrompt".to_owned(), Value::String(prompt.to_owned()));
            }
        }
    }
    if let Some(max_parallel) = input.max_parallel {
        raw.insert("maxParallel".to_owned(), Value::from(max_parallel));
    }
    if let Some(retention) = input.worktree_retention {
        match retention {
            None | Some(0) => {
                raw.remove("worktreeRetention");
            }
            Some(value) => {
                raw.insert("worktreeRetention".to_owned(), Value::from(value));
            }
        }
    }
    if let Some(value) = input.live_title_updates {
        match value {
            None => {
                raw.remove("liveTitleUpdates");
            }
            Some(flag) => {
                raw.insert("liveTitleUpdates".to_owned(), Value::Bool(flag));
            }
        }
    }
    if let Some(value) = input.review_gate {
        match value {
            None => {
                raw.remove("reviewGate");
            }
            Some(flag) => {
                raw.insert("reviewGate".to_owned(), Value::Bool(flag));
            }
        }
    }
    if let Some(limit) = input.memory_limit_mb {
        match limit {
            None | Some(0) => {
                raw.remove("memoryLimitMb");
            }
            Some(value) => {
                raw.insert("memoryLimitMb".to_owned(), Value::from(value));
            }
        }
    }
    if let Some(models_patch) = &input.default_models {
        let mut models = raw
            .get("defaultModels")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, patch) in [
            ("claude", &models_patch.claude),
            ("codex", &models_patch.codex),
            ("opencode", &models_patch.opencode),
            ("pi", &models_patch.pi),
        ] {
            if let Some(value) = patch {
                match value.as_deref().map(str::trim) {
                    None | Some("") => {
                        models.remove(key);
                    }
                    Some(model) => {
                        models.insert(key.to_owned(), Value::String(model.to_owned()));
                    }
                }
            }
        }
        if models.is_empty() {
            raw.remove("defaultModels");
        } else {
            raw.insert("defaultModels".to_owned(), Value::Object(models));
        }
    }
    if let Some(composer_patch) = &input.composer_defaults {
        apply_project_composer_defaults(&mut raw, composer_patch);
    }
    coducktor_core::workspace::config::atomic_write_json_sync(
        &repo_config_path_at(repo_root, state_home),
        &Value::Object(raw),
    )
    .map_err(io_err)?;
    Ok(config_response(repo_root, state_home))
}

// ---- workflow builder helpers -------------------------------------------------------------
// same name (see `save_workflow`'s own doc comment for why duplication, not sharing, is right
// here) ------------------------------------------------------------------------------------

fn workflow_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "chain".to_owned()
    } else {
        slug
    }
}

fn workflow_step_issue(steps: &[WorkflowStepDef]) -> Option<String> {
    for step in steps {
        if step.id.is_empty() {
            return Some("step id must not be empty".to_owned());
        }
        let has_command = step.command.as_ref().is_some_and(|value| !value.is_empty());
        let has_agent = step.prompt.as_ref().is_some_and(|value| !value.is_empty())
            || step.skill.as_ref().is_some_and(|value| !value.is_empty());
        if has_command == has_agent {
            return Some(format!(
                "step \"{}\" is either an agent step or a check step",
                step.id
            ));
        }
        if let Some(on_fail) = &step.on_fail
            && on_fail.max == 0
        {
            return Some(format!("step \"{}\": onFail.max must be positive", step.id));
        }
    }
    steps_issue(steps)
}

fn workflow_input(
    input: &SaveWorkflowInput,
) -> Result<(String, Option<String>, Vec<WorkflowStepDef>, bool), String> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("name must be between 1 and 80 characters".to_owned());
    }
    if input
        .description
        .as_ref()
        .is_some_and(|value| value.chars().count() > 2_000)
    {
        return Err("description must be at most 2000 characters".to_owned());
    }
    if input.steps.is_some() == input.skills.is_some() {
        return Err("provide either \"steps\" or \"skills\", not both".to_owned());
    }
    if input.steps.as_ref().is_some_and(|steps| steps.is_empty())
        || input.steps.as_ref().is_some_and(|steps| steps.len() > 8)
    {
        return Err("steps must contain between 1 and 8 entries".to_owned());
    }
    if input
        .skills
        .as_ref()
        .is_some_and(|skills| skills.is_empty())
        || input.skills.as_ref().is_some_and(|skills| skills.len() > 8)
    {
        return Err("skills must contain between 1 and 8 entries".to_owned());
    }
    let (steps, compact) = if let Some(skills) = &input.skills {
        let mut names = Vec::with_capacity(skills.len());
        for skill in skills {
            let skill = skill.trim();
            if skill.is_empty() {
                return Err("skills entries must not be empty".to_owned());
            }
            names.push(skill.to_owned());
        }
        (
            coducktor_core::workflows::types::skills_to_steps(&names),
            true,
        )
    } else {
        (input.steps.clone().unwrap_or_default(), false)
    };
    if let Some(issue) = workflow_step_issue(&steps) {
        return Err(issue);
    }
    Ok((
        name.to_owned(),
        input
            .description
            .as_ref()
            .filter(|value| !value.is_empty())
            .cloned(),
        steps,
        compact,
    ))
}

fn workflow_yaml(
    name: &str,
    description: Option<&str>,
    steps: &[WorkflowStepDef],
    compact: bool,
) -> Result<String, String> {
    let mut document = Map::new();
    document.insert("name".to_owned(), Value::String(name.to_owned()));
    if let Some(description) = description {
        document.insert(
            "description".to_owned(),
            Value::String(description.to_owned()),
        );
    }
    if compact {
        let skills = steps
            .iter()
            .filter_map(|step| step.skill.clone())
            .map(Value::String)
            .collect::<Vec<_>>();
        document.insert("skills".to_owned(), Value::Array(skills));
    } else {
        let steps = serde_json::to_value(steps).map_err(|error| error.to_string())?;
        document.insert("steps".to_owned(), steps);
    }
    serde_yaml_ng::to_string(&Value::Object(document)).map_err(|error| error.to_string())
}

// ---- agent-profile + provider-status helpers ----------------------------------------------
// functions of the same name (same non-sharing rationale as the workflow builder helpers
// above) --------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ResolvedAgentProfile {
    id: String,
    provider: Runner,
    label: String,
    config_dir: String,
    path: PathBuf,
    is_default: bool,
}

fn default_agent_profile(provider: Runner) -> ResolvedAgentProfile {
    let home = agent_home_paths(&ProcessEnv);
    let path = match provider {
        Runner::Claude => home.claude,
        Runner::Codex => home.codex,
        Runner::OpenCode => home.opencode_config,
        Runner::Pi => PathBuf::new(),
    };
    ResolvedAgentProfile {
        id: coducktor_contract::DEFAULT_AGENT_ACCOUNT_ID.to_owned(),
        provider,
        label: "Default".to_owned(),
        config_dir: path.to_string_lossy().into_owned(),
        path,
        is_default: true,
    }
}

fn resolved_agent_profile(account: &AgentAccount) -> ResolvedAgentProfile {
    ResolvedAgentProfile {
        id: account.id.clone(),
        provider: account.provider,
        label: if account.label.is_empty() {
            account.id.clone()
        } else {
            account.label.clone()
        },
        config_dir: account.config_dir.clone(),
        path: expand_tilde(&account.config_dir, &ProcessEnv),
        is_default: false,
    }
}

fn profile_file_defs(provider: Runner) -> &'static [(&'static str, &'static str)] {
    match provider {
        Runner::Claude => &[
            ("claude.user.settings", "settings.json"),
            ("claude.user.memory", "CLAUDE.md"),
        ],
        Runner::Codex => &[
            ("codex.user.config", "config.toml"),
            ("codex.user.memory", "AGENTS.md"),
        ],
        Runner::OpenCode => &[
            ("opencode.user.config", "opencode.json"),
            ("opencode.user.memory", "AGENTS.md"),
        ],
        Runner::Pi => &[],
    }
}

fn profile_files(profile: &ResolvedAgentProfile) -> Vec<AgentAccountFile> {
    profile_file_defs(profile.provider)
        .iter()
        .map(|(id, name)| {
            let path = profile.path.join(name);
            AgentAccountFile {
                id: (*id).to_owned(),
                label: (*name).to_owned(),
                exists: std::fs::metadata(&path).is_ok(),
                path: path.to_string_lossy().into_owned(),
            }
        })
        .collect()
}

fn profile_dir_state(profile: &ResolvedAgentProfile) -> (bool, bool) {
    let Ok(entries) = std::fs::read_dir(&profile.path) else {
        return (false, false);
    };
    let names = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<std::collections::BTreeSet<_>>();
    let markers: &[&str] = match profile.provider {
        Runner::Claude => &[".claude.json", "settings.json", "projects", "sessions"],
        Runner::Codex => &["auth.json", "config.toml"],
        Runner::OpenCode | Runner::Pi => &[],
    };
    (true, markers.iter().any(|marker| names.contains(*marker)))
}

fn agent_profile_wire(profile: &ResolvedAgentProfile) -> AgentProfile {
    let (exists, looks_valid) = profile_dir_state(profile);
    AgentProfile {
        id: profile.id.clone(),
        provider: profile.provider,
        label: profile.label.clone(),
        config_dir: profile.config_dir.clone(),
        path: profile.path.to_string_lossy().into_owned(),
        exists,
        looks_valid,
        is_default: profile.is_default,
        status: None,
        files: profile_files(profile),
    }
}

fn selection_wire(
    selection: &coducktor_core::workspace::agent_accounts::AgentAccountSelection,
) -> coducktor_contract::AgentAccountSelection {
    coducktor_contract::AgentAccountSelection {
        claude: selection.claude.clone(),
        codex: selection.codex.clone(),
        opencode: selection.opencode.clone(),
        pi: selection.pi.clone(),
    }
}

fn selection_empty(
    selection: &coducktor_core::workspace::agent_accounts::AgentAccountSelection,
) -> bool {
    selection.claude.is_none()
        && selection.codex.is_none()
        && selection.opencode.is_none()
        && selection.pi.is_none()
        && selection.extra.is_empty()
}

fn set_profile_selection(
    selection: &mut coducktor_core::workspace::agent_accounts::AgentAccountSelection,
    provider: Runner,
    profile_id: Option<String>,
) {
    match provider {
        Runner::Claude => selection.claude = profile_id,
        Runner::Codex => selection.codex = profile_id,
        Runner::OpenCode => selection.opencode = profile_id,
        Runner::Pi => selection.pi = profile_id,
    }
}

fn agent_profiles_response() -> AgentProfilesResponse {
    let store = coducktor_core::workspace::agent_accounts::load_agent_accounts(
        &agent_accounts_path(&ProcessEnv),
    );
    let mut profiles = Vec::new();
    for provider in PROVIDER_IDS {
        profiles.push(agent_profile_wire(&default_agent_profile(provider)));
        profiles.extend(
            store
                .accounts
                .iter()
                .filter(|account| account.provider == provider)
                .map(|account| agent_profile_wire(&resolved_agent_profile(account))),
        );
    }
    let selections = store
        .selections
        .iter()
        .map(|(root, selection)| (root.clone(), selection_wire(selection)))
        .collect::<BTreeMap<_, _>>();
    AgentProfilesResponse {
        editable: true,
        profiles,
        profile_capable_providers: vec![Runner::Claude, Runner::Codex],
        selections,
        defaults: selection_wire(&store.defaults),
    }
}

fn profile_path_error(config_dir: &str) -> Option<String> {
    if has_control_chars(config_dir) {
        return Some("folder must not contain control characters".to_owned());
    }
    let expanded = expand_tilde(config_dir, &ProcessEnv);
    if !is_absolute_config_dir(&expanded.to_string_lossy(), cfg!(windows)) {
        return Some(format!("folder must be an absolute path: {config_dir}"));
    }
    None
}

fn same_profile_dir(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    left.canonicalize().ok() == right.canonicalize().ok() && left.exists() && right.exists()
}

fn profile_conflict(
    store: &coducktor_core::workspace::agent_accounts::AgentAccountStore,
    provider: Runner,
    path: &Path,
    except_id: Option<&str>,
) -> Option<String> {
    let default = default_agent_profile(provider);
    if same_profile_dir(path, &default.path) {
        return Some("that is already this agent's default folder".to_owned());
    }
    store
        .accounts
        .iter()
        .filter(|account| account.provider == provider && Some(account.id.as_str()) != except_id)
        .find_map(|account| {
            let candidate = expand_tilde(&account.config_dir, &ProcessEnv);
            same_profile_dir(path, &candidate)
                .then(|| format!("that folder is already used by \"{}\"", account.label))
        })
}

/// `project_id: Some("default")` names the boot project even when it isn't (yet) registered in
/// `~/.coducktor/config.json` — same sentinel `boot_project_id` already returns as its own
/// fallback.
fn project_root_for_agent_selection(repo_root: &Path, project_id: Option<&str>) -> Option<PathBuf> {
    match project_id {
        None => None,
        Some("default") => Some(
            repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf()),
        ),
        Some(id) => {
            let config = load_workspace_config(
                &coducktor_core::paths::workspace_config_path(&ProcessEnv),
                &ProcessEnv,
            );
            config
                .projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| {
                    Path::new(&project.root)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(&project.root))
                })
        }
    }
}

fn account_by_route_id(accounts_path: &Path, id: &str) -> Option<ResolvedAgentProfile> {
    if let Some(provider) = id.strip_prefix("default:").and_then(|name| match name {
        "claude" => Some(Runner::Claude),
        "codex" => Some(Runner::Codex),
        "opencode" => Some(Runner::OpenCode),
        "pi" => Some(Runner::Pi),
        _ => None,
    }) {
        return Some(default_agent_profile(provider));
    }
    coducktor_core::workspace::agent_accounts::load_agent_accounts(accounts_path)
        .accounts
        .into_iter()
        .find(|account| account.id == id)
        .map(|account| resolved_agent_profile(&account))
}

const RESERVED_ACCOUNT_SLUG_IDS: &[&str] = &["default", "new", "settings", "api", "p", "assets"];

fn account_slug(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
}

/// Allocate an account id; account ids share the same
/// slug-collision-avoidance scheme (including the `project` fallback for an unslugifiable label)
/// project ids use.
fn allocate_account_id(value: &str, taken: &std::collections::BTreeSet<String>) -> String {
    let base = {
        let slug = account_slug(value);
        let slug = slug.trim_matches('-').chars().take(64).collect::<String>();
        if slug.is_empty() {
            "project".to_owned()
        } else {
            slug
        }
    };
    if !taken.contains(&base) && !RESERVED_ACCOUNT_SLUG_IDS.contains(&base.as_str()) {
        return base;
    }
    let mut suffix_number = 2;
    loop {
        let suffix = format!("-{suffix_number}");
        let prefix = base.chars().take(64 - suffix.len()).collect::<String>();
        let candidate = format!("{prefix}{suffix}");
        if !taken.contains(&candidate) && !RESERVED_ACCOUNT_SLUG_IDS.contains(&candidate.as_str()) {
            return candidate;
        }
        suffix_number += 1;
    }
}

fn provider_status_response() -> ProviderStatusResponse {
    let config = load_workspace_config(
        &coducktor_core::paths::workspace_config_path(&ProcessEnv),
        &ProcessEnv,
    );
    let locked = provider_models_locked();
    let providers = PROVIDER_IDS
        .into_iter()
        .map(|provider| {
            let mut status = if locked {
                ProviderStatus {
                    provider,
                    status: ProviderConnectionState::Connected,
                    enabled: Some(true),
                    hint: None,
                    auth_failure_id: None,
                    profile_id: None,
                }
            } else {
                provider_status_for_profile(&default_agent_profile(provider))
            };
            status.enabled = Some(locked || !config.disabled_providers.contains(&provider));
            status
        })
        .collect();
    ProviderStatusResponse { providers }
}

/// Build the stable connected-provider fallback order while degrading past missing CLIs and
/// disabled providers.
fn effective_requested_runner(
    authored: Option<RunnerSelection>,
    configured: RunnerSelection,
) -> RunnerSelection {
    authored.unwrap_or(configured)
}

/// One candidate's raw evaluation before ranking. `score` is only meaningful when `eligible`;
/// [`finalize_routing_decision`] never reads it otherwise.
struct CandidateEval {
    runner: Runner,
    eligible: bool,
    reason: coducktor_contract::RoutingReasonCode,
    score: Option<i64>,
}

/// A route key stable across a process's lifetime — not the executable path, which an
/// environment override can change without changing what route this candidate represents.
fn auto_route_key(runner: Runner) -> String {
    let name = match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    };
    format!("{name}:default")
}

/// Turn raw per-candidate evaluations into a [`RoutingDecision`]: eligible candidates are
/// ranked best-score-first with the top one marked `Selected` and the rest `Considered`;
/// ineligible candidates keep their evaluated reason. Shared by both the connectivity-only and
/// quota-aware candidate builders so a decision's shape never drifts from the `Vec<Runner>` an
/// older caller derives from it.
fn finalize_routing_decision(evals: Vec<CandidateEval>) -> coducktor_contract::RoutingDecision {
    use coducktor_contract::{ConsideredCandidate, RouteSelection, RoutingReasonCode};

    let (mut eligible, ineligible): (Vec<_>, Vec<_>) =
        evals.into_iter().partition(|candidate| candidate.eligible);
    eligible.sort_by_key(|candidate| std::cmp::Reverse(candidate.score.unwrap_or(i64::MIN)));

    let selected = eligible.first().map(|candidate| RouteSelection {
        runner: candidate.runner,
        profile_id: "default".to_owned(),
        upstream_provider: None,
        model: None,
        reasoning_effort: None,
        route_key: auto_route_key(candidate.runner),
    });
    let considered = eligible
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| ConsideredCandidate {
            route_key: auto_route_key(candidate.runner),
            runner: candidate.runner,
            profile_id: "default".to_owned(),
            model: None,
            eligible: true,
            // The top candidate wins outright; every other eligible candidate keeps whatever
            // reason it was evaluated with — `Considered` when nothing counted against it, or a
            // specific caveat (e.g. `UnknownUsage`) when one does.
            reason: if index == 0 {
                RoutingReasonCode::Selected
            } else {
                candidate.reason
            },
            score: candidate.score,
        })
        .chain(ineligible.into_iter().map(|candidate| ConsideredCandidate {
            route_key: auto_route_key(candidate.runner),
            runner: candidate.runner,
            profile_id: "default".to_owned(),
            model: None,
            eligible: false,
            reason: candidate.reason,
            score: None,
        }))
        .collect();
    coducktor_contract::RoutingDecision {
        selected,
        considered,
        retry_at: None,
        generation: 0,
    }
}

fn connection_eval(status: &ProviderStatusResponse, runner: Runner) -> CandidateEval {
    use coducktor_contract::RoutingReasonCode;

    let provider = status.providers.iter().find(|p| p.provider == runner);
    let (eligible, reason) = match provider {
        Some(provider) if provider.enabled != Some(true) => (false, RoutingReasonCode::Disabled),
        Some(provider) => match provider.status {
            ProviderConnectionState::Connected => (true, RoutingReasonCode::Considered),
            ProviderConnectionState::NotInstalled => (false, RoutingReasonCode::NotInstalled),
            ProviderConnectionState::Disconnected | ProviderConnectionState::Unknown => {
                (false, RoutingReasonCode::Disconnected)
            }
        },
        None => (false, RoutingReasonCode::NotInstalled),
    };
    CandidateEval {
        runner,
        eligible,
        reason,
        score: None,
    }
}

/// Connectivity-only Auto candidates — every enabled, connected runner is eligible with no
/// preference among them. Used when no usage snapshot is available at all.
fn connectivity_routing_decision(
    status: &ProviderStatusResponse,
) -> coducktor_contract::RoutingDecision {
    let evals = [Runner::Claude, Runner::Codex, Runner::OpenCode]
        .into_iter()
        .map(|runner| connection_eval(status, runner))
        .collect();
    finalize_routing_decision(evals)
}

/// The candidates a decision judged eligible, best-ranked first — the order the `Vec<Runner>`
/// call sites (candidate list, `Auto`'s own resolved-runner pick) have always needed.
fn routing_decision_runners(decision: &coducktor_contract::RoutingDecision) -> Vec<Runner> {
    decision
        .considered
        .iter()
        .filter(|candidate| candidate.eligible)
        .map(|candidate| candidate.runner)
        .collect()
}

/// Quota-aware provider selection deliberately ranks trustworthy available capacity above an
/// unknown account. This is the missing behavior that otherwise made the legacy Claude-first
/// fallback win even while Codex had a fresh, unused weekly window.
fn quota_aware_routing_decision(
    status: &ProviderStatusResponse,
    usage: &WorkspaceUsageResponse,
    policy: &coducktor_core::workspace::config::QuotaRouting,
) -> coducktor_contract::RoutingDecision {
    use coducktor_contract::RoutingReasonCode;

    let candidates = [Runner::Claude, Runner::Codex, Runner::OpenCode];
    let evals = candidates
        .into_iter()
        .map(|runner| {
            let connection = connection_eval(status, runner);
            if !connection.eligible {
                return connection;
            }
            let (provider, provider_policy) = match runner {
                Runner::Claude => (QuotaProvider::Claude, &policy.claude),
                Runner::Codex => (QuotaProvider::Codex, &policy.codex),
                Runner::OpenCode => (QuotaProvider::OpenCode, &policy.opencode),
                Runner::Pi => unreachable!("Pi is never offered as an Auto candidate"),
            };
            if !provider_policy.enabled {
                return CandidateEval {
                    runner,
                    eligible: false,
                    reason: RoutingReasonCode::Disabled,
                    score: None,
                };
            }
            let snapshot = usage
                .providers
                .iter()
                .find(|snapshot| snapshot.provider == provider && snapshot.profile_id == "default")
                .or_else(|| {
                    usage
                        .providers
                        .iter()
                        .find(|snapshot| snapshot.provider == provider)
                });
            let health = snapshot
                .map(|snapshot| snapshot.health)
                .unwrap_or(ProviderUsageHealth::Unknown);
            let (eligible, reason) = match health {
                ProviderUsageHealth::Available => (true, RoutingReasonCode::Considered),
                ProviderUsageHealth::Unknown
                    if policy.unknown_usage_policy
                        == coducktor_contract::UnknownUsagePolicy::AllowWithPenalty =>
                {
                    (true, RoutingReasonCode::UnknownUsage)
                }
                ProviderUsageHealth::Unknown => (false, RoutingReasonCode::UnknownUsage),
                ProviderUsageHealth::SoftExhausted => (false, RoutingReasonCode::ReservedQuota),
                ProviderUsageHealth::HardExhausted | ProviderUsageHealth::Unavailable => {
                    (false, RoutingReasonCode::HardExhausted)
                }
                ProviderUsageHealth::AuthError => (false, RoutingReasonCode::AuthError),
            };
            if !eligible {
                return CandidateEval {
                    runner,
                    eligible,
                    reason,
                    score: None,
                };
            }
            // A health match above already excluded `Unknown` without the penalty policy, so an
            // eligible-but-Unknown candidate here always carries the allow-with-penalty score
            // handicap baked into `health_rank`.
            let health_rank = if health == ProviderUsageHealth::Available {
                2_i64
            } else {
                1_i64
            };
            let headroom = snapshot
                .into_iter()
                .flat_map(|snapshot| &snapshot.windows)
                .filter_map(|window| window.used_percent)
                .map(|used| (100.0 - used.clamp(0.0, 100.0)).round() as i64)
                .min()
                .unwrap_or(-1);
            // Earlier in the configured order outranks later; not listed outranks nothing, so it
            // gets the lowest, zero, value. Bounded by the provider list's own length (at most a
            // handful of entries), unlike `priority`, which is user-configured and unbounded.
            let order = policy
                .provider_order
                .iter()
                .position(|candidate| *candidate == provider)
                .map(|position| (policy.provider_order.len() - position) as i64)
                .unwrap_or(0);
            // A single weighted sum standing in for the previous four-key lexicographic sort
            // (health, headroom, priority, order): each tier's multiplier is wide enough that the
            // tier below it, even at its clamped maximum, can never bleed into the tier above.
            let score = health_rank * 1_000_000_000_000
                + headroom.clamp(-1, 1_000) * 1_000_000_000
                + (provider_policy.priority as i64).clamp(0, 999_999) * 1_000
                + order.clamp(0, 999);
            CandidateEval {
                runner,
                eligible: true,
                reason,
                score: Some(score),
            }
        })
        .collect();
    finalize_routing_decision(evals)
}

fn provider_models_locked() -> bool {
    std::env::var("DUCK_AGENT_MODELS_LOCKED").is_ok_and(|value| value == "1")
}

fn provider_executable(provider: Runner) -> String {
    let (env_name, default) = match provider {
        Runner::Claude => ("DUCK_CLAUDE_BIN", "claude"),
        Runner::Codex => ("DUCK_CODEX_BIN", "codex"),
        Runner::OpenCode => ("DUCK_OPENCODE_BIN", "opencode"),
        Runner::Pi => ("DUCK_PI_BIN", "pi"),
    };
    std::env::var(env_name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn provider_probe_args(provider: Runner) -> &'static [&'static str] {
    match provider {
        Runner::Claude => &["auth", "status", "--json"],
        Runner::Codex => &["login", "status"],
        Runner::OpenCode => &["auth", "list"],
        Runner::Pi => &["--list-models"],
    }
}

fn provider_install_hint(provider: Runner) -> &'static str {
    match provider {
        Runner::Claude => "Install Claude Code, then run `claude auth login`.",
        Runner::Codex => "Install the Codex CLI, then run `codex login`.",
        Runner::OpenCode => "Install OpenCode, then run `opencode auth login`.",
        Runner::Pi => "Install pi, then run `pi /login`.",
    }
}

/// The default profile's interactive login subcommand, spawnable as `program` + these args in a
/// fresh terminal. `None` when the provider has no such one-shot command — pi's `/login` is a
/// slash command typed inside its own interactive session, not something this seam can drive.
fn provider_login_args(provider: Runner) -> Option<Vec<String>> {
    match provider {
        Runner::Claude => Some(vec!["auth".to_owned(), "login".to_owned()]),
        Runner::Codex => Some(vec!["login".to_owned()]),
        Runner::OpenCode => Some(vec!["auth".to_owned(), "login".to_owned()]),
        Runner::Pi => None,
    }
}

fn runner_display_name(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "Claude Code",
        Runner::Codex => "Codex",
        Runner::OpenCode => "OpenCode",
        Runner::Pi => "pi",
    }
}

fn provider_state_from_output(
    provider: Runner,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> Option<ProviderConnectionState> {
    let combined = format!("{stdout}\n{stderr}");
    let lower = combined.to_ascii_lowercase();
    match provider {
        Runner::Claude => {
            let logged_in = serde_json::from_str::<Value>(stdout)
                .ok()?
                .get("loggedIn")
                .and_then(Value::as_bool)?;
            if logged_in && exit_code == Some(0) {
                Some(ProviderConnectionState::Connected)
            } else if !logged_in && exit_code == Some(1) {
                Some(ProviderConnectionState::Disconnected)
            } else {
                None
            }
        }
        Runner::Codex => {
            let connected = lower.lines().any(|line| {
                let line = line.trim();
                line.starts_with("logged in using ")
            });
            let disconnected = lower
                .lines()
                .any(|line| line.trim() == "not logged in" || line.contains("run codex login"));
            match (connected, disconnected, exit_code) {
                (true, false, Some(0)) => Some(ProviderConnectionState::Connected),
                (false, true, Some(1)) => Some(ProviderConnectionState::Disconnected),
                _ => None,
            }
        }
        Runner::OpenCode => {
            // OpenCode's human-readable output is often decorated with box-drawing
            // characters, for example `└  1 credentials`. Find the numeric/credential
            // pair anywhere on the line instead of requiring the count to be column zero.
            let mut counts = lower
                .lines()
                .filter_map(|line| {
                    let words = line.split_whitespace().collect::<Vec<_>>();
                    words.windows(2).find_map(|pair| {
                        let count = pair[0].parse::<u64>().ok()?;
                        pair[1].starts_with("credential").then_some(count)
                    })
                })
                .collect::<Vec<_>>();
            if counts.len() != 1 || exit_code != Some(0) {
                return None;
            }
            Some(if counts.remove(0) > 0 {
                ProviderConnectionState::Connected
            } else {
                ProviderConnectionState::Disconnected
            })
        }
        Runner::Pi => {
            if exit_code != Some(0) {
                return None;
            }
            if lower.lines().any(|line| {
                line.split_whitespace().collect::<Vec<_>>()
                    == [
                        "provider", "model", "context", "max-out", "thinking", "images",
                    ]
            }) {
                Some(ProviderConnectionState::Connected)
            } else if lower.contains("no models available") && lower.contains("/login") {
                Some(ProviderConnectionState::Disconnected)
            } else {
                None
            }
        }
    }
}

fn provider_status_for_profile(profile: &ResolvedAgentProfile) -> ProviderStatus {
    let profile_id = (!profile.is_default).then(|| profile.id.clone());
    if std::env::var("DUCK_DRY_RUN").is_ok_and(|value| value == "1") {
        return ProviderStatus {
            provider: profile.provider,
            status: ProviderConnectionState::Connected,
            enabled: None,
            hint: None,
            auth_failure_id: None,
            profile_id,
        };
    }
    let executable = provider_executable(profile.provider);
    let mut command = Command::new(&executable);
    command.args(provider_probe_args(profile.provider));
    if !profile.is_default {
        match profile.provider {
            Runner::Claude => {
                command.env("CLAUDE_CONFIG_DIR", &profile.path);
            }
            Runner::Codex => {
                command.env("CODEX_HOME", &profile.path);
            }
            Runner::OpenCode | Runner::Pi => {}
        }
    }
    let result = command.output();
    let (status, hint) = match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            ProviderConnectionState::NotInstalled,
            Some(provider_install_hint(profile.provider).to_owned()),
        ),
        Err(_) => (
            ProviderConnectionState::Unknown,
            Some("Authentication could not be verified. Try again.".to_owned()),
        ),
        Ok(output) => {
            let state = provider_state_from_output(
                profile.provider,
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
                output.status.code(),
            );
            match state {
                Some(state) => (state, None),
                None => (
                    ProviderConnectionState::Unknown,
                    Some("Authentication could not be verified. Try again.".to_owned()),
                ),
            }
        }
    };
    ProviderStatus {
        provider: profile.provider,
        status,
        enabled: None,
        hint,
        auth_failure_id: None,
        profile_id,
    }
}

fn capped_json_file(path: &Path) -> Option<Value> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > 2 * 1024 * 1024 {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn identity_text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn agent_profile_details(profile: &ResolvedAgentProfile) -> AgentAccountDetailsResponse {
    if matches!(profile.provider, Runner::OpenCode | Runner::Pi) {
        return AgentAccountDetailsResponse {
            available: false,
            reason: Some(
                "OpenCode keeps its login outside its config folder, so coducktor cannot read it."
                    .to_owned(),
            ),
            fields: Vec::new(),
        };
    }
    let path = match profile.provider {
        Runner::Claude => {
            if profile.is_default && std::env::var("CLAUDE_CONFIG_DIR").is_err() {
                profile
                    .path
                    .parent()
                    .unwrap_or(profile.path.as_path())
                    .join(".claude.json")
            } else {
                profile.path.join(".claude.json")
            }
        }
        Runner::Codex => profile.path.join("auth.json"),
        Runner::OpenCode | Runner::Pi => profile.path.clone(),
    };
    let Some(document) = capped_json_file(&path) else {
        return AgentAccountDetailsResponse {
            available: false,
            reason: Some("Not signed in on this account yet — use Connect.".to_owned()),
            fields: Vec::new(),
        };
    };
    let mut fields = Vec::new();
    match profile.provider {
        Runner::Claude => {
            if let Some(account) = document.get("oauthAccount").and_then(Value::as_object) {
                for (label, key) in [
                    ("Email", "emailAddress"),
                    ("Name", "displayName"),
                    ("Organization", "organizationName"),
                    ("Role", "organizationRole"),
                    ("Seat", "seatTier"),
                    ("Billing", "billingType"),
                ] {
                    if let Some(value) = identity_text(account.get(key)) {
                        fields.push(coducktor_contract::AgentAccountDetailField {
                            label: label.to_owned(),
                            value,
                        });
                    }
                }
            }
        }
        Runner::Codex => {
            if document
                .get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
            {
                fields.push(coducktor_contract::AgentAccountDetailField {
                    label: "Login".to_owned(),
                    value: "API key".to_owned(),
                });
            }
        }
        Runner::OpenCode | Runner::Pi => {}
    }
    if fields.is_empty() {
        AgentAccountDetailsResponse {
            available: false,
            reason: Some("Could not read this account’s details.".to_owned()),
            fields,
        }
    } else {
        AgentAccountDetailsResponse {
            available: true,
            reason: None,
            fields,
        }
    }
}

/// Best-effort "open with the OS default app" —
/// `account_open_default` exactly (fire-and-forget `spawn`, success = the process launched, not
/// that the user actually saw a window).
fn account_open_default(path: &Path) -> bool {
    let (program, args) = if cfg!(target_os = "macos") {
        ("open", vec![path.to_string_lossy().into_owned()])
    } else if cfg!(target_os = "windows") {
        ("explorer", vec![path.to_string_lossy().into_owned()])
    } else {
        ("xdg-open", vec![path.to_string_lossy().into_owned()])
    };
    Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn api_run(record: coducktor_contract::RunRecord) -> ApiRun {
    ApiRun {
        record,
        usage: None,
    }
}

#[allow(clippy::too_many_lines)]
fn run_index_entry(project_id: &str, run: coducktor_contract::RunRecord) -> RunIndexEntry {
    let model_usage = model_usage_breakdown(&run.steps);
    RunIndexEntry {
        project_id: project_id.to_owned(),
        id: run.id,
        title: run.title,
        title_summary: run.title_summary,
        title_origin: run.title_origin,
        status: run.status,
        activity: run.activity,
        created_at: run.created_at,
        updated_at: run.updated_at,
        finished_at: run.finished_at,
        seen_at: run.seen_at,
        archived: run.archived,
        archived_at: run.archived_at,
        prompt_preview: prompt_preview(&run.task),
        auto_resume_at: run.auto_resume_at,
        workflow: run.workflow,
        branch: run.branch,
        started_at: run.started_at,
        pull_request_url: run.pull_request_url,
        referenced_pull_request_url: run.referenced_pull_request_url,
        pr_number: run.pr_number,
        issue_number: run.issue_number,
        referenced_issue_url: run.referenced_issue_url,
        marker_refs: run.marker_refs,
        cost_usd: run.cost_usd,
        peak_rss_bytes: run.peak_rss_bytes,
        peak_proc_count: run.peak_proc_count,
        usage: None,
        runner: run.runner,
        model: run.model,
        model_usage,
        model_identity: run.model_identity,
        reasoning_effort: None,
    }
}

/// A run's cost broken down by the concrete model each step actually used, as a percentage of
/// the run's total step cost. `None` when every step shares one model — auto-failover across
/// providers mid-run is the case this exists to make visible, not the common single-model run.
/// Steps a runner never priced (`cost_usd: None`, e.g. a runner with no cost telemetry) are
/// excluded from both the per-model total and the denominator, so an unpriced step neither
/// dilutes nor is misrepresented in the percentages of the steps that were priced.
fn model_usage_breakdown(
    steps: &[coducktor_contract::runs::StepState],
) -> Option<Vec<ModelUsageEntry>> {
    let mut totals: Vec<(
        String,
        Option<coducktor_contract::ConcreteReasoningEffort>,
        f64,
    )> = Vec::new();
    for step in steps {
        let (Some(model), Some(cost)) = (step.model_identity.as_deref(), step.cost_usd) else {
            continue;
        };
        match totals.iter_mut().find(|(existing, reasoning, _)| {
            existing == model && *reasoning == step.reasoning_effort
        }) {
            Some((_, _, total)) => *total += cost,
            None => totals.push((model.to_owned(), step.reasoning_effort, cost)),
        }
    }
    let distinct_models = totals
        .iter()
        .map(|(model, _, _)| model.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if distinct_models < 2 {
        return None;
    }
    let total: f64 = totals.iter().map(|(_, _, cost)| cost).sum();
    if total <= 0.0 {
        return None;
    }
    Some(
        totals
            .into_iter()
            .map(|(model, reasoning_effort, cost)| ModelUsageEntry {
                model,
                reasoning_effort,
                pct: cost / total * 100.0,
            })
            .collect(),
    )
}

/// Coducktor's own recorded token/cost totals per provider, summed across every step of every
/// run in the shared in-memory snapshot (no disk I/O — the same cache `runs_index` reads)
/// created since the start of the current UTC calendar month. This is the "Coducktor-recorded"
/// fallback the workspace usage panel shows for a provider whose own quota API can't be queried
/// (Claude has none at all; others degrade to it on a failed probe) — evidence the app already
/// has from running the work, not a new telemetry source.
fn coducktor_recorded_consumption(
    run_snapshot: &BTreeMap<String, BTreeMap<String, coducktor_contract::RunRecord>>,
) -> Vec<(Runner, UsageAggregate)> {
    let month_start = {
        let now = coducktor_core::time::now_iso8601();
        format!("{}-01T00:00:00.000Z", &now[..7])
    };
    // `Runner` is a small, Copy, non-`Ord` enum — a handful of linear-scanned entries is both
    // simpler and no slower than a map for at most four runners.
    let mut totals: Vec<(Runner, f64, f64, f64)> = Vec::new();
    for project_runs in run_snapshot.values() {
        for run in project_runs.values() {
            if run.created_at < month_start {
                continue;
            }
            for step in &run.steps {
                let Some(backend) = step.backend.or(run.runner) else {
                    continue;
                };
                let entry = match totals.iter_mut().find(|(runner, ..)| *runner == backend) {
                    Some(entry) => entry,
                    None => {
                        totals.push((backend, 0.0, 0.0, 0.0));
                        totals.last_mut().expect("just pushed")
                    }
                };
                entry.1 += step.input_tokens.unwrap_or(0.0);
                entry.2 += step.output_tokens.unwrap_or(0.0);
                entry.3 += step.cost_usd.unwrap_or(0.0);
            }
        }
    }
    totals
        .into_iter()
        .filter(|(_, input, output, cost)| *input > 0.0 || *output > 0.0 || *cost > 0.0)
        .map(|(runner, input_tokens, output_tokens, cost_usd)| {
            (
                runner,
                UsageAggregate {
                    scope: UsageAggregateScope::CoducktorOnly,
                    period_start: Some(month_start.clone()),
                    period_end: None,
                    input_tokens: Some(input_tokens),
                    output_tokens: Some(output_tokens),
                    reasoning_tokens: None,
                    cache_tokens: None,
                    cost_usd: (cost_usd > 0.0).then_some(cost_usd),
                },
            )
        })
        .collect()
}

/// Keep the workspace index useful without copying an unbounded task body into it.
/// Character iteration makes the bound safe for UTF-8 and the source text remains in RunRecord.
fn prompt_preview(task: &str) -> Option<String> {
    const MAX_CHARS: usize = 240;
    let collapsed = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut chars = collapsed.chars();
    let mut preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    Some(preview)
}

fn boot_project_id(
    config: &coducktor_core::workspace::config::WorkspaceConfig,
    repo_root: &Path,
) -> String {
    let canonical_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    config
        .projects
        .iter()
        .find(|project| {
            PathBuf::from(&project.root).canonicalize().ok().as_ref() == Some(&canonical_root)
        })
        .map(|project| project.id.clone())
        .unwrap_or_else(|| "default".to_owned())
}

/// Resolve each project's live Git status/
/// branch on every call; this view deliberately does not cache Git status.
#[cfg(test)]
fn resolve_scope_root(
    repo_root: &Path,
    scope: &Scope,
    projects: &[coducktor_core::workspace::config::WorkspaceProject],
) -> Result<PathBuf, EngineError> {
    let root = match scope {
        Scope::Workspace => repo_root.to_owned(),
        Scope::Project(id) if id == "default" => repo_root.to_owned(),
        Scope::Project(id) => projects
            .iter()
            .find(|project| project.id == *id)
            .map(|project| PathBuf::from(&project.root))
            .ok_or(EngineError::NotFound)?,
    };
    root.canonicalize()
        .map_err(|error| EngineError::Unavailable {
            reason: format!("project root {} is unavailable: {error}", root.display()),
        })
}

fn same_project_root(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn project_entry(
    project: &coducktor_core::workspace::config::WorkspaceProject,
) -> ProjectListEntry {
    let root = Path::new(&project.root);
    let (status, branch) = if !root.is_dir() {
        (ProjectStatus::Missing, None)
    } else if git_output(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true") {
        (
            ProjectStatus::Ok,
            git_output(root, &["branch", "--show-current"]),
        )
    } else {
        (ProjectStatus::NotGit, None)
    };
    ProjectListEntry {
        id: project.id.clone(),
        name: project.name.clone(),
        root: project.root.clone(),
        added_at: project.added_at.clone(),
        last_opened_at: project.last_opened_at.clone(),
        source: match project.source {
            coducktor_core::workspace::config::ProjectSource::Local => ProjectSource::Local,
            coducktor_core::workspace::config::ProjectSource::Checkout => ProjectSource::Checkout,
        },
        status,
        branch,
        forge: None,
        repo_url: None,
        max_parallel: project.max_parallel.map(|value| value as f64),
        tags: project.tags.clone(),
    }
}

fn repo_ui_state_path(repo_root: &Path, state_home: &Path) -> PathBuf {
    project_state_dir_in(state_home, repo_root).join("ui-state.json")
}

fn read_repo_ui_state(repo_root: &Path, state_home: &Path) -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(repo_ui_state_path(repo_root, state_home)) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn health_payload(repo_root: &Path, version: &str, probe_backends: bool) -> HealthResponse {
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let repo = repo_info_at(repo_root).map(|info| RepoInfo {
        root: info.root,
        branch: info.branch,
        remote: info.remote,
    });
    let forge_available = github_remote(repo_root).is_some();
    HealthResponse {
        version: version.to_owned(),
        repo_root: repo_root_str,
        repo,
        checks: [
            (BackendCheckName::Claude, "claude"),
            (BackendCheckName::Codex, "codex"),
            (BackendCheckName::OpenCode, "opencode"),
            (BackendCheckName::Pi, "pi"),
            (BackendCheckName::Gh, "gh"),
            (BackendCheckName::Git, "git"),
        ]
        .into_iter()
        .map(|(name, binary)| {
            if probe_backends {
                backend_check(name, binary)
            } else {
                backend_presence_check(name, binary)
            }
        })
        .collect(),
        default_runner: RunnerSelection::Auto,
        forge: Some(ForgeInfo {
            kind: ForgeKind::GitHub,
            available: Some(forge_available),
            reason: (!forge_available)
                .then(|| "GitHub is unavailable for this repository".to_owned()),
        }),
        capabilities: Capabilities {},
        projects: vec![HealthProject {
            id: "default".to_owned(),
            name: repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project")
                .to_owned(),
        }],
        boot_project: "default".to_owned(),
    }
}

/// Find a GitHub remote regardless of its configured name. This intentionally only parses local
/// Git config; authentication/network checks remain the forge driver's degraded capability.
fn github_remote(repo_root: &Path) -> Option<String> {
    let names = git_capture(repo_root, &["remote"])
        .ok()?
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    names.into_iter().find_map(|name| {
        let remote = git_capture(repo_root, &["remote", "get-url", name.as_str()])
            .ok()?
            .trim()
            .to_owned();
        resolve_forge(repo_root.to_path_buf(), Some(remote.as_str())).map(|_| remote)
    })
}

fn backend_check(name: BackendCheckName, binary: &str) -> BackendCheck {
    match Command::new(binary).arg("--version").output() {
        Ok(output) if output.status.success() => BackendCheck {
            name,
            available: true,
            version: first_line(&output.stdout).or_else(|| first_line(&output.stderr)),
            hint: None,
        },
        Ok(_) => BackendCheck {
            name,
            available: false,
            version: None,
            hint: Some(format!("{binary} --version failed")),
        },
        Err(error) => BackendCheck {
            name,
            available: false,
            version: None,
            hint: Some(if error.kind() == std::io::ErrorKind::NotFound {
                format!("{binary} CLI not found")
            } else {
                error.to_string()
            }),
        },
    }
}

fn backend_presence_check(name: BackendCheckName, binary: &str) -> BackendCheck {
    let env_name = match name {
        BackendCheckName::Claude => Some("DUCK_CLAUDE_BIN"),
        BackendCheckName::Codex => Some("DUCK_CODEX_BIN"),
        BackendCheckName::OpenCode => Some("DUCK_OPENCODE_BIN"),
        BackendCheckName::Pi => Some("DUCK_PI_BIN"),
        BackendCheckName::Gh | BackendCheckName::Git => None,
    };
    let override_present = env_name
        .and_then(std::env::var_os)
        .is_some_and(|path| !path.is_empty() && Path::new(&path).is_file());
    let dry_run_fallback = std::env::var("DUCK_DRY_RUN").is_ok_and(|value| value == "1")
        && matches!(name, BackendCheckName::Claude | BackendCheckName::Pi);
    let available = override_present || dry_run_fallback || executable_on_path(binary);
    BackendCheck {
        name,
        available,
        version: None,
        hint: (!available).then(|| format!("{binary} CLI not found")),
    }
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    first_line(&output.stdout)
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
}

