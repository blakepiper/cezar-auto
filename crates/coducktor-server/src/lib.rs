//! The temporary HTTP boundary used while the Rust engine and terminal UI are separate
//! processes.
//!
//! B9 ports the Node service behind the same `/api/v1` contract.  This crate deliberately
//! owns transport concerns only: route handlers validate a request, delegate to the core
//! services, and serialize a contract value.  The first B9 slice exposes health and the
//! request-origin perimeter so subsequent route-family commits have one stable shell to
//! extend.  The entire crate is deleted at C2 when the TUI switches to an in-process engine.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::HeaderName;
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, HOST, ORIGIN};
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use coducktor_contract::{
    AgentConfigFile, AgentConfigFileContent, AgentConfigFormat, AgentConfigKind,
    AgentConfigListing, AgentConfigScope, AgentConfigTracked, ApiRun, BackendCheck,
    BackendCheckName, Capabilities, ConfigResponse, ContinueInput, CreateRunInput,
    CreateRunResponse, DeleteRunResponse, DeleteWorkflowResponse, ForgeInfo, ForgeKind,
    HealthProject, HealthResponse, IdeDirectoryQuery, IdeDirectoryResponse, IdeEntry, IdeEntryType,
    IdeFileInput, IdeFileQuery, IdeFileResponse, MarkAllReadResponse, MessageInput,
    ParseWorkflowInput, ParsedWorkflow, PatchRunInput, ProjectListEntry, ProjectSource,
    ProjectStatus, ProjectsResponse, RegisterProjectInput, RegisterProjectResponse,
    RemoveProjectResponse, RepoInfo, RunRecord, Runner, RunnerModels, RunnerSelection,
    SaveWorkflowInput, SaveWorkflowResponse, SetAgentConfigInput, SetConfigInput,
    SetWorkspaceConfigInput, SetWorkspaceUiStateInput, StartTodoResponse, TodoItem, UiState,
    UpdateProjectInput, UpdateProjectResponse, UserMcpListing, WorkflowStepDef, WorkflowsResponse,
    WorkspaceConfigResponse,
};
use coducktor_core::config::load_config;
use coducktor_core::handoff::followups_enabled;
use coducktor_core::paths::{ProcessEnv, coducktor_home_dir};
use coducktor_core::skills::discover_skills;
use coducktor_core::time::now_iso8601;
use coducktor_core::workflows::load::load_workflows;
use coducktor_core::workflows::run::{
    ContinueOptions, RunManager, StartRunInput as CoreStartRunInput,
};
use coducktor_core::workflows::types::{
    parse_workflow_file_doc, quick_task_workflow, skills_to_steps, steps_issue,
};
use coducktor_core::workspace::config::{
    ProjectSource as CoreProjectSource, WorkspaceConfig, WorkspaceProject, atomic_write_json_sync,
    load_workspace_config, merge_write_workspace_config,
};
use coducktor_core::workspace::ui_state::{
    merge_write_workspace_ui_state, read_workspace_ui_state,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const HEALTH_PATH: &str = "/api/v1/health";
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Configuration shared by the route families.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub repo_root: PathBuf,
    pub version: String,
}

impl ServerConfig {
    pub fn new(repo_root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            version: version.into(),
        }
    }
}

/// Immutable state for the B9 transport shell. Mutable route-family state is added behind
/// this `Arc` as each family lands; handlers never keep state in global statics.
#[derive(Clone)]
pub struct ServerState {
    config: Arc<ServerConfig>,
    manager: Arc<Mutex<RunManager>>,
    workspace_dir: Arc<PathBuf>,
}

impl ServerState {
    pub fn new(config: ServerConfig) -> Self {
        let manager = RunManager::for_repo(&config.repo_root);
        Self::with_manager(config, manager)
    }

    pub fn with_manager(config: ServerConfig, manager: RunManager) -> Self {
        let workspace_dir = coducktor_home_dir(&ProcessEnv);
        Self::with_manager_and_workspace_dir(config, manager, workspace_dir)
    }

    pub fn with_manager_and_workspace_dir(
        config: ServerConfig,
        manager: RunManager,
        workspace_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            manager: Arc::new(Mutex::new(manager)),
            workspace_dir: Arc::new(workspace_dir.into()),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn manager(&self) -> &Arc<Mutex<RunManager>> {
        &self.manager
    }

    fn workspace_config_path(&self) -> PathBuf {
        self.workspace_dir.join("config.json")
    }

    fn workspace_ui_state_path(&self) -> PathBuf {
        self.workspace_dir.join("ui-state.json")
    }
}

/// Build the versioned HTTP application.
pub fn router(config: ServerConfig) -> Router {
    let state = ServerState::new(config);
    router_with_state(state)
}

/// Construct the application around a caller-owned manager. Route-family tests use this to
/// seed durable records without reaching through HTTP; the production path uses [`router`].
pub fn router_with_state(state: ServerState) -> Router {
    Router::new()
        .route(HEALTH_PATH, get(health))
        .route("/api/v1/runs", get(list_runs).post(create_run))
        .route(
            "/api/v1/runs/archive-finished",
            axum::routing::post(archive_finished),
        )
        .route("/api/v1/runs/read-all", axum::routing::post(mark_all_read))
        .route(
            "/api/v1/runs/{id}",
            get(get_run).patch(patch_run).delete(delete_run),
        )
        .route(
            "/api/v1/runs/{id}/archive",
            axum::routing::post(archive_run),
        )
        .route("/api/v1/runs/{id}/read", axum::routing::post(read_run))
        .route("/api/v1/runs/{id}/unread", axum::routing::post(unread_run))
        .route("/api/v1/runs/{id}/cancel", axum::routing::post(cancel_run))
        .route(
            "/api/v1/runs/{id}/continue",
            axum::routing::post(continue_run),
        )
        .route("/api/v1/runs/{id}/finish", axum::routing::post(finish_run))
        .route(
            "/api/v1/runs/{id}/messages",
            axum::routing::post(send_message),
        )
        .route(
            "/api/v1/runs/{id}/auto-resume",
            axum::routing::delete(cancel_auto_resume),
        )
        // Project-scoped routes have the same default and boot-project aliases as the Node
        // service.  The scope guard below admits only `default` and this server's boot project.
        .route("/api/v1/p/{project}/runs", get(list_runs).post(create_run))
        .route(
            "/api/v1/p/{project}/runs/archive-finished",
            axum::routing::post(archive_finished),
        )
        .route(
            "/api/v1/p/{project}/runs/read-all",
            axum::routing::post(mark_all_read),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}",
            get(get_run).patch(patch_run).delete(delete_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/archive",
            axum::routing::post(archive_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/read",
            axum::routing::post(read_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/unread",
            axum::routing::post(unread_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/cancel",
            axum::routing::post(cancel_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/continue",
            axum::routing::post(continue_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/finish",
            axum::routing::post(finish_run),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/messages",
            axum::routing::post(send_message),
        )
        .route(
            "/api/v1/p/{project}/runs/{id}/auto-resume",
            axum::routing::delete(cancel_auto_resume),
        )
        .route(
            "/api/v1/projects",
            get(list_projects).post(register_project),
        )
        .route(
            "/api/v1/projects/{project_id}",
            axum::routing::patch(update_project).delete(remove_project),
        )
        .route(
            "/api/v1/workspace/config",
            get(get_workspace_config).put(update_workspace_config),
        )
        .route(
            "/api/v1/workspace/ui-state",
            get(get_workspace_ui_state).put(update_workspace_ui_state),
        )
        .route("/api/v1/workspace/usage", get(get_workspace_usage))
        .route("/api/v1/skills", get(list_skills))
        .route("/api/v1/p/{project}/skills", get(list_scoped_skills))
        .route("/api/v1/workflows", get(list_workflows).post(save_workflow))
        .route(
            "/api/v1/p/{project}/workflows",
            get(list_scoped_workflows).post(save_scoped_workflow),
        )
        .route(
            "/api/v1/workflows/{name}",
            axum::routing::delete(delete_workflow),
        )
        .route(
            "/api/v1/p/{project}/workflows/{name}",
            axum::routing::delete(delete_scoped_workflow),
        )
        .route(
            "/api/v1/workflows/parse",
            axum::routing::post(parse_workflow),
        )
        .route(
            "/api/v1/p/{project}/workflows/parse",
            axum::routing::post(parse_scoped_workflow),
        )
        .route("/api/v1/ui-state", get(get_ui_state).put(update_ui_state))
        .route(
            "/api/v1/p/{project}/ui-state",
            get(get_scoped_ui_state).put(update_scoped_ui_state),
        )
        .route("/api/v1/todos", get(list_todos))
        .route("/api/v1/p/{project}/todos", get(list_scoped_todos))
        .route("/api/v1/todos/{id}", axum::routing::delete(delete_todo))
        .route(
            "/api/v1/p/{project}/todos/{id}",
            axum::routing::delete(delete_scoped_todo),
        )
        .route("/api/v1/todos/{id}/start", axum::routing::post(start_todo))
        .route(
            "/api/v1/p/{project}/todos/{id}/start",
            axum::routing::post(start_scoped_todo),
        )
        .route("/api/v1/config", get(get_config).put(update_config))
        .route(
            "/api/v1/p/{project}/config",
            get(get_scoped_config).put(update_scoped_config),
        )
        .route("/api/v1/agent-config", get(list_agent_config))
        .route(
            "/api/v1/p/{project}/agent-config",
            get(list_scoped_agent_config),
        )
        .route(
            "/api/v1/agent-config/{id}",
            get(get_agent_config).put(update_agent_config),
        )
        .route(
            "/api/v1/p/{project}/agent-config/{id}",
            get(get_scoped_agent_config).put(update_scoped_agent_config),
        )
        .route("/api/v1/ide/tree", get(list_ide_directory))
        .route(
            "/api/v1/p/{project}/ide/tree",
            get(list_scoped_ide_directory),
        )
        .route("/api/v1/ide/file", get(read_ide_file).put(write_ide_file))
        .route(
            "/api/v1/p/{project}/ide/file",
            get(read_scoped_ide_file).put(write_scoped_ide_file),
        )
        .fallback(not_found)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, request_origin_guard))
}

/// Start serving an already-bound listener. The caller owns binding so tests can select a free
/// port and the future CLI can keep port policy outside the temporary server crate.
pub async fn serve(
    listener: tokio::net::TcpListener,
    config: ServerConfig,
) -> Result<(), std::io::Error> {
    axum::serve(listener, router(config)).await
}

async fn health(State(state): State<ServerState>) -> Response {
    let payload = health_payload(state.config());
    let mut response = Json(payload).into_response();
    // Health is the one intentionally CORS-readable discovery route. The host guard still runs
    // first, so this does not make DNS rebinding readable.
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not found")
}

fn workflow_repo_root(state: &ServerState) -> PathBuf {
    state.config.repo_root.clone()
}

fn repo_data_dir(state: &ServerState) -> PathBuf {
    state.config.repo_root.join(".ai").join("coducktor")
}

fn repo_ui_state_path(state: &ServerState) -> PathBuf {
    repo_data_dir(state).join("ui-state.json")
}

fn read_repo_ui_state(state: &ServerState) -> Map<String, Value> {
    let Ok(raw) = fs::read_to_string(repo_ui_state_path(state)) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn validate_ui_state_input(input: Value) -> Result<Map<String, Value>, String> {
    let Some(object) = input.as_object() else {
        return Err("ui-state must be a JSON object".to_owned());
    };
    if object.len() > 200 {
        return Err("ui-state has too many keys (max 200)".to_owned());
    }
    let copy = Value::Object(object.clone());
    serde_json::from_value::<UiState>(copy).map_err(|error| error.to_string())?;
    if let Some(usage) = object.get("skillUsage") {
        let Some(entries) = usage.as_object() else {
            return Err("skillUsage must be an object".to_owned());
        };
        if entries.len() > 200 {
            return Err("skillUsage must have at most 200 entries".to_owned());
        }
        for (name, count) in entries {
            if name.is_empty() || name.chars().count() > 200 {
                return Err("skillUsage keys must be between 1 and 200 characters".to_owned());
            }
            let Some(count) = count.as_i64() else {
                return Err("skillUsage counts must be integers from 0 to 1000000".to_owned());
            };
            if !(0..=1_000_000).contains(&count) {
                return Err("skillUsage counts must be integers from 0 to 1000000".to_owned());
            }
        }
    }
    Ok(object.clone())
}

fn merge_repo_ui_state(state: &ServerState, input: Value) -> Result<Map<String, Value>, String> {
    let patch = validate_ui_state_input(input)?;
    let mut merged = read_repo_ui_state(state);
    merged.extend(patch);
    atomic_write_json_sync(&repo_ui_state_path(state), &Value::Object(merged.clone()))
        .map_err(|error| error.to_string())?;
    Ok(merged)
}

fn repo_config_path(state: &ServerState) -> PathBuf {
    repo_data_dir(state).join("config.json")
}

fn read_repo_config(state: &ServerState) -> Map<String, Value> {
    let Ok(raw) = fs::read_to_string(repo_config_path(state)) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn config_models_locked(state: &ServerState, config: &coducktor_core::config::RepoConfig) -> bool {
    std::env::var("CEZ_AGENT_MODELS_LOCKED").is_ok_and(|value| value == "1")
        || workspace_config(state).models_locked == Some(true)
        || config.models_locked == Some(true)
}

fn config_response(state: &ServerState) -> ConfigResponse {
    let workspace = workspace_config(state);
    let config = load_config(&state.config.repo_root, &workspace.agent_defaults);
    let models_locked = config_models_locked(state, &config);
    ConfigResponse {
        base_branch: config.base_branch,
        default_runner: config.default_runner,
        system_prompt: config.system_prompt,
        default_models: if models_locked {
            RunnerModels::default()
        } else {
            config.default_models
        },
        models_locked,
        max_parallel: config.max_parallel,
        memory_limit_mb: config.memory_limit_mb,
        worktree_retention: config.worktree_retention,
        live_title_updates: config.live_title_updates,
        review_gate: config.review_gate,
    }
}

fn parse_set_config_input(input: Value) -> Result<(SetConfigInput, Map<String, Value>), String> {
    let Some(object) = input.as_object().cloned() else {
        return Err("config must be a JSON object".to_owned());
    };
    let typed = serde_json::from_value::<SetConfigInput>(Value::Object(object.clone()))
        .map_err(|error| error.to_string())?;
    if typed
        .base_branch
        .as_ref()
        .and_then(|value| value.as_ref())
        .is_some_and(|value| {
            let trimmed = value.trim();
            trimmed.is_empty() || trimmed.chars().count() > 200
        })
    {
        return Err("baseBranch must be between 1 and 200 characters".to_owned());
    }
    if typed
        .system_prompt
        .as_ref()
        .and_then(|value| value.as_ref())
        .is_some_and(|value| value.trim().chars().count() > 20_000)
    {
        return Err("systemPrompt must be at most 20000 characters".to_owned());
    }
    if typed
        .max_parallel
        .is_some_and(|value| !(1..=16).contains(&value))
    {
        return Err("maxParallel must be an integer from 1 to 16".to_owned());
    }
    if typed
        .memory_limit_mb
        .flatten()
        .is_some_and(|value| value > 1_048_576)
    {
        return Err("memoryLimitMb must be an integer from 0 to 1048576".to_owned());
    }
    if typed
        .worktree_retention
        .flatten()
        .is_some_and(|value| value > 1000)
    {
        return Err("worktreeRetention must be an integer from 0 to 1000".to_owned());
    }
    if object.get("defaultModels").is_some_and(Value::is_null) {
        return Err("defaultModels must be an object".to_owned());
    }
    if let Some(models) = object.get("defaultModels") {
        let Some(models) = models.as_object() else {
            return Err("defaultModels must be an object".to_owned());
        };
        for key in ["claude", "codex", "opencode", "pi"] {
            if let Some(value) = models.get(key)
                && value
                    .as_str()
                    .is_some_and(|model| model.trim().chars().count() > 200)
            {
                return Err("model names must be at most 200 characters".to_owned());
            } else if models
                .get(key)
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err("model names must be strings or null".to_owned());
            }
        }
    }
    Ok((typed, object))
}

fn config_patch_value_is_null(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).is_some_and(Value::is_null)
}

#[derive(Debug, Clone, Copy)]
enum AgentConfigPath {
    ClaudeUserSettings,
    ClaudeProjectSettings,
    ClaudeLocalSettings,
    ClaudeProjectMcp,
    ClaudeUserMemory,
    ClaudeProjectMemory,
    ClaudeLocalMemory,
    CodexUserConfig,
    CodexProjectConfig,
    CodexUserMemory,
    OpenCodeUserConfig,
    OpenCodeProjectConfig,
    OpenCodeUserMemory,
    ProjectAgents,
}

#[derive(Debug, Clone, Copy)]
struct AgentConfigDefinition {
    id: &'static str,
    runners: &'static [Runner],
    kind: AgentConfigKind,
    scope: AgentConfigScope,
    label: &'static str,
    format: AgentConfigFormat,
    tracked: AgentConfigTracked,
    seeded: bool,
    holds_mcp: bool,
    path: AgentConfigPath,
    docs_url: &'static str,
}

const CLAUDE_RUNNER: &[Runner] = &[Runner::Claude];
const CODEX_RUNNER: &[Runner] = &[Runner::Codex];
const OPENCODE_RUNNER: &[Runner] = &[Runner::OpenCode];
const CODEX_OPENCODE_RUNNERS: &[Runner] = &[Runner::Codex, Runner::OpenCode];

const AGENT_CONFIG_DEFINITIONS: &[AgentConfigDefinition] = &[
    AgentConfigDefinition {
        id: "claude.user.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.claude/settings.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeUserSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.project.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: ".claude/settings.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeProjectSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.local.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Local,
        label: ".claude/settings.local.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Gitignored,
        seeded: true,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeLocalSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.project.mcp",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Mcp,
        scope: AgentConfigScope::Project,
        label: ".mcp.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::ClaudeProjectMcp,
        docs_url: "https://code.claude.com/docs/en/mcp",
    },
    AgentConfigDefinition {
        id: "claude.user.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.claude/CLAUDE.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeUserMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "claude.project.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Project,
        label: "CLAUDE.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeProjectMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "claude.local.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Local,
        label: "CLAUDE.local.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Gitignored,
        seeded: true,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeLocalMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "codex.user.config",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.codex/config.toml",
        format: AgentConfigFormat::Toml,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::CodexUserConfig,
        docs_url: "https://developers.openai.com/codex/config-reference",
    },
    AgentConfigDefinition {
        id: "codex.project.config",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: ".codex/config.toml",
        format: AgentConfigFormat::Toml,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::CodexProjectConfig,
        docs_url: "https://developers.openai.com/codex/config-reference",
    },
    AgentConfigDefinition {
        id: "codex.user.memory",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.codex/AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::CodexUserMemory,
        docs_url: "https://developers.openai.com/codex/guides/agents-md",
    },
    AgentConfigDefinition {
        id: "opencode.user.config",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.config/opencode/opencode.json",
        format: AgentConfigFormat::JsonC,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::OpenCodeUserConfig,
        docs_url: "https://opencode.ai/docs/config/",
    },
    AgentConfigDefinition {
        id: "opencode.project.config",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: "opencode.json",
        format: AgentConfigFormat::JsonC,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::OpenCodeProjectConfig,
        docs_url: "https://opencode.ai/docs/config/",
    },
    AgentConfigDefinition {
        id: "opencode.user.memory",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.config/opencode/AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::OpenCodeUserMemory,
        docs_url: "https://opencode.ai/docs/rules/",
    },
    AgentConfigDefinition {
        id: "project.agents",
        runners: CODEX_OPENCODE_RUNNERS,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Project,
        label: "AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ProjectAgents,
        docs_url: "https://opencode.ai/docs/rules/",
    },
];

fn agent_config_definition(id: &str) -> Option<&'static AgentConfigDefinition> {
    AGENT_CONFIG_DEFINITIONS
        .iter()
        .find(|definition| definition.id == id)
}

fn resolve_agent_config_path(definition: &AgentConfigDefinition, repo_root: &Path) -> PathBuf {
    let homes = coducktor_core::paths::agent_home_paths(&ProcessEnv);
    match definition.path {
        AgentConfigPath::ClaudeUserSettings => homes.claude.join("settings.json"),
        AgentConfigPath::ClaudeProjectSettings => repo_root.join(".claude/settings.json"),
        AgentConfigPath::ClaudeLocalSettings => repo_root.join(".claude/settings.local.json"),
        AgentConfigPath::ClaudeProjectMcp => repo_root.join(".mcp.json"),
        AgentConfigPath::ClaudeUserMemory => homes.claude.join("CLAUDE.md"),
        AgentConfigPath::ClaudeProjectMemory => repo_root.join("CLAUDE.md"),
        AgentConfigPath::ClaudeLocalMemory => repo_root.join("CLAUDE.local.md"),
        AgentConfigPath::CodexUserConfig => homes.codex.join("config.toml"),
        AgentConfigPath::CodexProjectConfig => repo_root.join(".codex/config.toml"),
        AgentConfigPath::CodexUserMemory => homes.codex.join("AGENTS.md"),
        AgentConfigPath::OpenCodeUserConfig => homes.opencode_config.join("opencode.json"),
        AgentConfigPath::OpenCodeProjectConfig => repo_root.join("opencode.json"),
        AgentConfigPath::OpenCodeUserMemory => homes.opencode_config.join("AGENTS.md"),
        AgentConfigPath::ProjectAgents => repo_root.join("AGENTS.md"),
    }
}

fn config_hash(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agent_config_content(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
) -> Result<AgentConfigFileContent, String> {
    let path = resolve_agent_config_path(definition, repo_root);
    match fs::read(&path) {
        Ok(bytes) => {
            let content = String::from_utf8(bytes.clone()).map_err(|error| error.to_string())?;
            Ok(AgentConfigFileContent {
                id: definition.id.to_owned(),
                path: path.to_string_lossy().into_owned(),
                exists: true,
                content,
                version: Some(config_hash(&bytes)),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AgentConfigFileContent {
            id: definition.id.to_owned(),
            path: path.to_string_lossy().into_owned(),
            exists: false,
            content: String::new(),
            version: None,
        }),
        Err(error) => Err(error.to_string()),
    }
}

fn jsonc_without_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte >= 0x80
                && let Some(character) = input[index..].chars().next()
            {
                output.push(character);
                index += character.len_utf8();
                continue;
            }
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            if index < bytes.len() {
                output.push('\n');
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            output.push(' ');
            output.push(' ');
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                output.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                index += 1;
            }
            if index < bytes.len() {
                output.push(' ');
                output.push(' ');
                index += 2;
            }
            continue;
        }
        if byte >= 0x80
            && let Some(character) = input[index..].chars().next()
        {
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        output.push(byte as char);
        index += 1;
    }
    output
}

fn validate_agent_config(content: &str, format: AgentConfigFormat) -> Result<(), String> {
    if content.trim().is_empty() || matches!(format, AgentConfigFormat::Markdown) {
        return Ok(());
    }
    match format {
        AgentConfigFormat::Json => serde_json::from_str::<Value>(content)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::JsonC => serde_json::from_str::<Value>(&jsonc_without_comments(content))
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::Toml => toml::from_str::<toml::Value>(content)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::Markdown => Ok(()),
    }
}

fn claude_state_path() -> PathBuf {
    let homes = coducktor_core::paths::agent_home_paths(&ProcessEnv);
    let default_home = coducktor_core::paths::real_home_dir(&ProcessEnv).join(".claude");
    if std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        || homes.claude != default_home
    {
        homes.claude.join(".claude.json")
    } else {
        homes.claude.parent().map_or_else(
            || PathBuf::from(".claude.json"),
            |parent| parent.join(".claude.json"),
        )
    }
}

fn user_mcp_listing() -> UserMcpListing {
    let path = claude_state_path();
    let path_string = path.to_string_lossy().into_owned();
    let Ok(metadata) = fs::metadata(&path) else {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: true,
        };
    };
    if metadata.len() > 2 * 1024 * 1024 {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: false,
        };
    }
    let Ok(raw) = fs::read_to_string(&path) else {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: false,
        };
    };
    let servers = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.get("mcpServers").cloned())
        .and_then(|value| value.as_object().cloned())
        .map(|servers| servers.into_iter().map(|(name, _)| name).collect())
        .unwrap_or_default();
    UserMcpListing {
        path: path_string,
        servers,
        readable: true,
    }
}

fn agent_config_listing(repo_root: &Path) -> AgentConfigListing {
    let files = AGENT_CONFIG_DEFINITIONS
        .iter()
        .map(|definition| {
            let path = resolve_agent_config_path(definition, repo_root);
            let metadata = fs::metadata(&path).ok();
            let (exists, size, version) = match metadata {
                Some(metadata) => {
                    let version = fs::read(&path).ok().map(|bytes| config_hash(&bytes));
                    (true, metadata.len() as f64, version)
                }
                None => (false, 0.0, None),
            };
            AgentConfigFile {
                id: definition.id.to_owned(),
                runners: definition.runners.to_vec(),
                kind: definition.kind,
                scope: definition.scope,
                label: definition.label.to_owned(),
                path: path.to_string_lossy().into_owned(),
                format: definition.format,
                tracked: definition.tracked,
                seeded: definition.seeded,
                holds_mcp: definition.holds_mcp,
                precedence: "Vendor-documented precedence; coducktor writes the file verbatim."
                    .to_owned(),
                hot_reload: None,
                docs_url: definition.docs_url.to_owned(),
                exists,
                size,
                version,
                writable: true,
                read_only_reason: None,
            }
        })
        .collect();
    AgentConfigListing {
        editable: true,
        files,
        user_mcp: Some(user_mcp_listing()),
    }
}

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
        (skills_to_steps(&names), true)
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

async fn list_skills(State(state): State<ServerState>) -> Response {
    let skills = discover_skills(&workflow_repo_root(&state), &ProcessEnv);
    Json(skills).into_response()
}

async fn list_scoped_skills(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    list_skills(State(state)).await
}

async fn list_workflows(State(state): State<ServerState>) -> Response {
    let (workflows, issues) = load_workflows(&workflow_repo_root(&state));
    Json(WorkflowsResponse { workflows, issues }).into_response()
}

async fn list_scoped_workflows(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    list_workflows(State(state)).await
}

async fn save_workflow_at(state: &ServerState, input: SaveWorkflowInput) -> Response {
    let (name, description, steps, compact) = match workflow_input(&input) {
        Ok(value) => value,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error),
    };
    let root = workflow_repo_root(state);
    let directory = root.join(coducktor_core::workflows::load::WORKFLOWS_DIR);
    let path = directory.join(format!("{}.yaml", workflow_slug(&name)));
    let yaml = match workflow_yaml(&name, description.as_deref(), &steps, compact) {
        Ok(yaml) => yaml,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error),
    };
    if let Err(error) = fs::create_dir_all(&directory) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    if input.overwrite.unwrap_or(false) {
        options.truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("workflow file already exists: {}", path.display()),
                    "exists": true,
                })),
            )
                .into_response();
        }
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    if let Err(error) = file.write_all(yaml.as_bytes()) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (
        StatusCode::CREATED,
        Json(SaveWorkflowResponse {
            path: path.to_string_lossy().into_owned(),
            name,
        }),
    )
        .into_response()
}

async fn save_workflow(
    State(state): State<ServerState>,
    Json(input): Json<SaveWorkflowInput>,
) -> Response {
    save_workflow_at(&state, input).await
}

async fn save_scoped_workflow(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(input): Json<SaveWorkflowInput>,
) -> Response {
    save_workflow_at(&state, input).await
}

async fn delete_workflow_at(state: &ServerState, name: String) -> Response {
    let root = workflow_repo_root(state);
    let (workflows, _) = load_workflows(&root);
    let Some(workflow) = workflows.into_iter().find(|workflow| workflow.name == name) else {
        return error_response(StatusCode::NOT_FOUND, &format!("unknown workflow: {name}"));
    };
    let Some(path) = workflow.path else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "built-in workflows cannot be deleted",
        );
    };
    let directory = root.join(coducktor_core::workflows::load::WORKFLOWS_DIR);
    let target = PathBuf::from(&path);
    if !target.starts_with(&directory) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "refusing to delete a file outside the workflows dir",
        );
    }
    if let Err(error) = fs::remove_file(&target) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    Json(DeleteWorkflowResponse {
        ok: true,
        path: target.to_string_lossy().into_owned(),
    })
    .into_response()
}

async fn delete_workflow(
    State(state): State<ServerState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    delete_workflow_at(&state, name).await
}

async fn delete_scoped_workflow(
    State(state): State<ServerState>,
    AxumPath((_project, name)): AxumPath<(String, String)>,
) -> Response {
    delete_workflow_at(&state, name).await
}

fn parse_workflow_input(input: ParseWorkflowInput) -> Result<ParsedWorkflow, String> {
    if input.yaml.trim().is_empty() || input.yaml.chars().count() > 100_000 {
        return Err("yaml must be between 1 and 100000 characters".to_owned());
    }
    let value: Value =
        serde_yaml_ng::from_str(&input.yaml).map_err(|error| format!("not valid YAML: {error}"))?;
    let (name, description, steps) = parse_workflow_file_doc(&value)?;
    Ok(ParsedWorkflow {
        name,
        description,
        steps,
    })
}

async fn parse_workflow(Json(input): Json<ParseWorkflowInput>) -> Response {
    match parse_workflow_input(input) {
        Ok(parsed) => Json(parsed).into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, &error),
    }
}

async fn parse_scoped_workflow(
    AxumPath(_project): AxumPath<String>,
    Json(input): Json<ParseWorkflowInput>,
) -> Response {
    parse_workflow(Json(input)).await
}

fn workspace_config(state: &ServerState) -> WorkspaceConfig {
    load_workspace_config(&state.workspace_config_path(), &ProcessEnv)
}

fn boot_project_id(config: &WorkspaceConfig, repo_root: &Path) -> String {
    let canonical_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    if let Some(project) = config.projects.iter().find(|project| {
        Path::new(&project.root)
            .canonicalize()
            .is_ok_and(|root| root == canonical_root)
    }) {
        return project.id.clone();
    }
    let taken = config
        .projects
        .iter()
        .map(|project| project.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    allocate_project_id(
        repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project"),
        &taken,
    )
}

const RESERVED_PROJECT_IDS: &[&str] = &["default", "new", "settings", "api", "p", "assets"];

fn project_slug(value: &str) -> String {
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

fn allocate_project_id(value: &str, taken: &std::collections::BTreeSet<String>) -> String {
    let base = {
        let slug = project_slug(value);
        let slug = slug.trim_matches('-').chars().take(64).collect::<String>();
        if slug.is_empty() {
            "project".to_owned()
        } else {
            slug
        }
    };
    if !taken.contains(&base) && !RESERVED_PROJECT_IDS.contains(&base.as_str()) {
        return base;
    }
    let mut suffix_number = 2;
    loop {
        let suffix = format!("-{suffix_number}");
        let prefix = base.chars().take(64 - suffix.len()).collect::<String>();
        let candidate = format!("{prefix}{suffix}");
        if !taken.contains(&candidate) && !RESERVED_PROJECT_IDS.contains(&candidate.as_str()) {
            return candidate;
        }
        suffix_number += 1;
    }
}

fn project_entry(project: &WorkspaceProject) -> ProjectListEntry {
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
            CoreProjectSource::Local => ProjectSource::Local,
            CoreProjectSource::Checkout => ProjectSource::Checkout,
        },
        status,
        branch,
        forge: None,
        repo_url: None,
        max_parallel: project.max_parallel.map(|value| value as f64),
        tags: project.tags.clone(),
    }
}

fn project_snapshot(config: &WorkspaceConfig, repo_root: &Path) -> (Vec<ProjectListEntry>, String) {
    let boot_id = boot_project_id(config, repo_root);
    (config.projects.iter().map(project_entry).collect(), boot_id)
}

async fn list_projects(State(state): State<ServerState>) -> Response {
    let config = workspace_config(&state);
    let (projects, boot_project) = project_snapshot(&config, &state.config.repo_root);
    Json(ProjectsResponse {
        projects,
        boot_project,
        projects_dir: config.projects_dir,
    })
    .into_response()
}

async fn register_project(
    State(state): State<ServerState>,
    Json(input): Json<RegisterProjectInput>,
) -> Response {
    let requested = input.root.trim();
    let requested_path = Path::new(requested);
    if requested.is_empty() || !requested_path.is_absolute() {
        return error_response(StatusCode::BAD_REQUEST, "root must be an absolute path");
    }
    let Ok(root) = requested_path.canonicalize() else {
        return error_response(StatusCode::BAD_REQUEST, "root must be a non-empty path");
    };
    if !root.is_dir() {
        return error_response(StatusCode::BAD_REQUEST, "root must be a directory");
    }
    let root_string = root.to_string_lossy().into_owned();
    let path = state.workspace_config_path();
    let config = load_workspace_config(&path, &ProcessEnv);
    if let Some(existing) = config.projects.iter().find(|project| {
        Path::new(&project.root)
            .canonicalize()
            .is_ok_and(|existing| existing == root)
    }) {
        return (
            StatusCode::CONFLICT,
            Json(RegisterProjectResponse {
                project: project_entry(existing),
                error: Some("project already registered".to_owned()),
            }),
        )
            .into_response();
    }
    let taken = config
        .projects
        .iter()
        .map(|project| project.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let id = allocate_project_id(
        root.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project"),
        &taken,
    );
    let now = now_iso8601();
    let project = WorkspaceProject {
        id,
        root: root_string,
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned(),
        added_at: now.clone(),
        last_opened_at: now,
        source: CoreProjectSource::Local,
        max_parallel: None,
        tags: None,
        extra: Map::new(),
    };
    let saved = project.clone();
    if let Err(error) = merge_write_workspace_config(&path, &ProcessEnv, move |config| {
        config.projects.push(saved);
    }) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    Json(RegisterProjectResponse {
        project: project_entry(&project),
        error: None,
    })
    .into_response()
}

async fn remove_project(
    State(state): State<ServerState>,
    AxumPath(project_id): AxumPath<String>,
) -> Response {
    let config_path = state.workspace_config_path();
    let config = load_workspace_config(&config_path, &ProcessEnv);
    let boot_id = boot_project_id(&config, &state.config.repo_root);
    let id = if project_id == "default" {
        boot_id.clone()
    } else {
        project_id
    };
    if !config.projects.iter().any(|project| project.id == id) {
        return error_response(StatusCode::NOT_FOUND, &format!("unknown project: {id}"));
    }
    if id == boot_id {
        return error_response(StatusCode::CONFLICT, "cannot remove the boot project");
    }
    if let Err(error) = merge_write_workspace_config(&config_path, &ProcessEnv, |config| {
        config.projects.retain(|project| project.id != id);
    }) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    Json(RemoveProjectResponse { removed: true, id }).into_response()
}

fn normalize_project_tags(tags: Option<Vec<String>>) -> Option<Vec<String>> {
    let mut tags = tags?
        .into_iter()
        .map(|tag| tag.trim().to_owned())
        .collect::<Vec<_>>();
    tags.retain(|tag| !tag.is_empty());
    tags.sort_by_key(|tag| tag.to_ascii_lowercase());
    tags.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    (!tags.is_empty()).then_some(tags)
}

fn validate_project_update(input: &UpdateProjectInput) -> Result<(), String> {
    if input.max_parallel.is_none() && input.tags.is_none() {
        return Err("specify maxParallel or tags".to_owned());
    }
    if let Some(Some(max_parallel)) = input.max_parallel
        && !(1..=16).contains(&max_parallel)
    {
        return Err("maxParallel must be an integer from 1 to 16".to_owned());
    }
    if let Some(Some(tags)) = &input.tags {
        if tags.len() > coducktor_contract::PROJECT_TAGS_MAX {
            return Err(format!(
                "tags must have at most {} entries",
                coducktor_contract::PROJECT_TAGS_MAX
            ));
        }
        if tags.iter().any(|tag| {
            let trimmed = tag.trim();
            trimmed.is_empty()
                || trimmed.chars().count() > coducktor_contract::PROJECT_TAG_MAX_LENGTH
        }) {
            return Err(format!(
                "tags must contain non-empty values of at most {} characters",
                coducktor_contract::PROJECT_TAG_MAX_LENGTH
            ));
        }
    }
    Ok(())
}

async fn update_project(
    State(state): State<ServerState>,
    AxumPath(project_id): AxumPath<String>,
    Json(input): Json<UpdateProjectInput>,
) -> Response {
    if let Err(error) = validate_project_update(&input) {
        return error_response(StatusCode::BAD_REQUEST, &error);
    }
    let config_path = state.workspace_config_path();
    let config = load_workspace_config(&config_path, &ProcessEnv);
    let boot_id = boot_project_id(&config, &state.config.repo_root);
    let id = if project_id == "default" {
        boot_id
    } else {
        project_id
    };
    if !config.projects.iter().any(|project| project.id == id) {
        return error_response(StatusCode::NOT_FOUND, &format!("unknown project: {id}"));
    }
    let mut updated = None;
    let max_parallel = input.max_parallel;
    let tags = input.tags;
    if let Err(error) = merge_write_workspace_config(&config_path, &ProcessEnv, |config| {
        if let Some(project) = config.projects.iter_mut().find(|project| project.id == id) {
            if let Some(value) = max_parallel {
                project.max_parallel = value;
            }
            if let Some(value) = tags.clone() {
                project.tags = normalize_project_tags(value);
            }
            updated = Some(project.clone());
        }
    }) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    let Some(updated) = updated else {
        return error_response(StatusCode::NOT_FOUND, &format!("unknown project: {id}"));
    };
    Json(UpdateProjectResponse {
        project: project_entry(&updated),
    })
    .into_response()
}

fn workspace_config_response(config: &WorkspaceConfig) -> WorkspaceConfigResponse {
    let models =
        config
            .agent_defaults
            .models
            .as_ref()
            .map(|models| coducktor_contract::RunnerModels {
                claude: models.claude.clone(),
                codex: models.codex.clone(),
                opencode: models.opencode.clone(),
                pi: models.pi.clone(),
            });
    WorkspaceConfigResponse {
        projects_dir: config.projects_dir.clone(),
        composer_defaults: coducktor_contract::ComposerDefaults {
            autonomous: config.composer_defaults.autonomous,
            worktree: config.composer_defaults.worktree,
            inherited_autonomous: coducktor_contract::InheritedAutonomous::Value(true),
            inherited_worktree: true,
        },
        resources: coducktor_contract::WorkspaceResources {
            max_parallel: config.resources.max_parallel,
            max_monitoring_sessions: config.resources.max_monitoring_sessions,
            monitoring_wake_interval_minutes: config.resources.monitoring_wake_interval_minutes,
            auto_resume_on_usage_limit: config.resources.auto_resume_on_usage_limit,
            intelligent_context_refresh: config.resources.intelligent_context_refresh,
            memory_limit_mb: config.resources.memory_limit_mb,
            worktree_retention_default: config.resources.worktree_retention_default,
        },
        quota_routing: config
            .quota_routing
            .enabled
            .then_some(coducktor_contract::QuotaRouting {
                enabled: true,
                provider_order: config.quota_routing.provider_order,
                unknown_usage_policy: config.quota_routing.unknown_usage_policy,
            }),
        agent_defaults: coducktor_contract::AgentDefaults {
            runner: config.agent_defaults.runner,
            models,
        },
    }
}

fn validate_workspace_config_input(input: &SetWorkspaceConfigInput) -> Result<(), String> {
    if let Some(projects_dir) = &input.projects_dir {
        let projects_dir = projects_dir.trim();
        if projects_dir.is_empty() || projects_dir.chars().count() > 4096 {
            return Err(
                "projectsDir must be a non-empty path of at most 4096 characters".to_owned(),
            );
        }
        if !projects_dir.starts_with('~') && !Path::new(projects_dir).is_absolute() {
            return Err(format!(
                "not writable: {projects_dir} is not an absolute path"
            ));
        }
    }
    if let Some(resources) = &input.resources {
        if let Some(value) = resources.max_parallel
            && !(1..=16).contains(&value)
        {
            return Err("maxParallel must be an integer from 1 to 16".to_owned());
        }
        if let Some(value) = resources.max_monitoring_sessions
            && value > 16
        {
            return Err("maxMonitoringSessions must be an integer from 0 to 16".to_owned());
        }
        if let Some(Some(value)) = resources.monitoring_wake_interval_minutes
            && !(1..=60).contains(&value)
        {
            return Err("monitoringWakeIntervalMinutes must be an integer from 1 to 60".to_owned());
        }
        if let Some(Some(value)) = resources.memory_limit_mb
            && value > 1_048_576
        {
            return Err("memoryLimitMb must be an integer from 0 to 1048576".to_owned());
        }
        if let Some(value) = resources.worktree_retention_default
            && value > 1000
        {
            return Err("worktreeRetentionDefault must be an integer from 0 to 1000".to_owned());
        }
    }
    if let Some(agent) = &input.agent_defaults
        && let Some(models) = &agent.models
        && [
            models.claude.as_ref(),
            models.codex.as_ref(),
            models.opencode.as_ref(),
            models.pi.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .any(|value| {
            let value = value.trim();
            value.is_empty() || value.chars().count() > 200
        })
    {
        return Err("model names must be between 1 and 200 characters".to_owned());
    }
    Ok(())
}

async fn get_workspace_config(State(state): State<ServerState>) -> Response {
    Json(workspace_config_response(&workspace_config(&state))).into_response()
}

async fn update_workspace_config(
    State(state): State<ServerState>,
    Json(input): Json<SetWorkspaceConfigInput>,
) -> Response {
    if let Err(error) = validate_workspace_config_input(&input) {
        return error_response(StatusCode::BAD_REQUEST, &error);
    }
    let path = state.workspace_config_path();
    let saved = match merge_write_workspace_config(&path, &ProcessEnv, |config| {
        if let Some(projects_dir) = &input.projects_dir {
            config.projects_dir = projects_dir.trim().to_owned();
        }
        if let Some(composer) = &input.composer_defaults {
            if let Some(autonomous) = composer.autonomous {
                config.composer_defaults.autonomous = autonomous;
            }
            if let Some(worktree) = composer.worktree {
                config.composer_defaults.worktree = worktree;
            }
        }
        if let Some(resources) = &input.resources {
            if let Some(value) = resources.max_parallel {
                config.resources.max_parallel = value;
            }
            if let Some(value) = resources.max_monitoring_sessions {
                config.resources.max_monitoring_sessions = value;
            }
            if let Some(value) = resources.monitoring_wake_interval_minutes {
                config.resources.monitoring_wake_interval_minutes = value;
            }
            if let Some(value) = resources.auto_resume_on_usage_limit {
                config.resources.auto_resume_on_usage_limit = value;
            }
            if let Some(value) = resources.intelligent_context_refresh {
                config.resources.intelligent_context_refresh = value;
            }
            if let Some(value) = resources.memory_limit_mb {
                config.resources.memory_limit_mb = value;
            }
            if let Some(value) = resources.worktree_retention_default {
                config.resources.worktree_retention_default = value;
            }
        }
        if let Some(agent) = &input.agent_defaults {
            if let Some(runner) = agent.runner {
                config.agent_defaults.runner = runner;
            }
            if let Some(models) = &agent.models {
                let has_patch = [&models.claude, &models.codex, &models.opencode, &models.pi]
                    .into_iter()
                    .any(Option::is_some);
                if has_patch {
                    let target = config
                        .agent_defaults
                        .models
                        .get_or_insert_with(Default::default);
                    if let Some(value) = &models.claude {
                        target.claude = value.as_ref().map(|value| value.trim().to_owned());
                    }
                    if let Some(value) = &models.codex {
                        target.codex = value.as_ref().map(|value| value.trim().to_owned());
                    }
                    if let Some(value) = &models.opencode {
                        target.opencode = value.as_ref().map(|value| value.trim().to_owned());
                    }
                    if let Some(value) = &models.pi {
                        target.pi = value.as_ref().map(|value| value.trim().to_owned());
                    }
                    if target.claude.is_none()
                        && target.codex.is_none()
                        && target.opencode.is_none()
                        && target.pi.is_none()
                        && target.extra.is_empty()
                    {
                        config.agent_defaults.models = None;
                    }
                }
            }
        }
        if let Some(quota) = &input.quota_routing
            && let Some(enabled) = quota.enabled
        {
            config.quota_routing.enabled = enabled;
        }
    }) {
        Ok(config) => config,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    Json(workspace_config_response(&saved)).into_response()
}

async fn get_workspace_ui_state(State(state): State<ServerState>) -> Response {
    Json(read_workspace_ui_state(&state.workspace_ui_state_path())).into_response()
}

async fn update_workspace_ui_state(
    State(state): State<ServerState>,
    Json(input): Json<SetWorkspaceUiStateInput>,
) -> Response {
    let path = state.workspace_ui_state_path();
    match merge_write_workspace_ui_state(&path, |state| {
        if input.sidebar.is_some() {
            state.sidebar = input.sidebar.clone();
        }
        if input.dismissed_provider_auth_failures.is_some() {
            state.dismissed_provider_auth_failures = input.dismissed_provider_auth_failures.clone();
        }
        if input.appearance.is_some() {
            state.appearance = input.appearance.clone();
        }
        if input.notifications.is_some() {
            state.notifications = input.notifications.clone();
        }
        if input.task_table.is_some() {
            state.task_table = input.task_table.clone();
        }
        if input.last_location.is_some() {
            state.last_location = input.last_location.clone();
        }
        if input.imported_skills.is_some() {
            state.imported_skills = input.imported_skills.clone();
        }
        state.extra.extend(input.extra.clone());
    }) {
        Ok(state) => Json(state).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn get_config(State(state): State<ServerState>) -> Response {
    Json(config_response(&state)).into_response()
}

async fn get_scoped_config(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    get_config(State(state)).await
}

async fn update_config(State(state): State<ServerState>, Json(input): Json<Value>) -> Response {
    let (typed, object) = match parse_set_config_input(input) {
        Ok(parsed) => parsed,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error),
    };
    let current = load_config(
        &state.config.repo_root,
        &workspace_config(&state).agent_defaults,
    );
    if config_models_locked(&state, &current) && object.contains_key("defaultModels") {
        return error_response(
            StatusCode::CONFLICT,
            "agent models are locked — configure the model in the native coding-agent settings",
        );
    }

    let mut raw = read_repo_config(&state);
    if let Some(value) = object.get("baseBranch") {
        if value.is_null() {
            raw.remove("baseBranch");
        } else if let Some(base_branch) = typed.base_branch.as_ref().and_then(Option::as_ref) {
            raw.insert(
                "baseBranch".to_owned(),
                Value::String(base_branch.trim().to_owned()),
            );
        }
    }
    if let Some(default_runner) = typed.default_runner {
        raw.insert(
            "defaultRunner".to_owned(),
            serde_json::to_value(default_runner).unwrap_or(Value::Null),
        );
    }
    if let Some(value) = object.get("systemPrompt") {
        if value.is_null()
            || typed
                .system_prompt
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|prompt| prompt.trim().is_empty())
        {
            raw.remove("systemPrompt");
        } else if let Some(prompt) = typed.system_prompt.as_ref().and_then(Option::as_ref) {
            raw.insert(
                "systemPrompt".to_owned(),
                Value::String(prompt.trim().to_owned()),
            );
        }
    }
    if let Some(max_parallel) = typed.max_parallel {
        raw.insert("maxParallel".to_owned(), Value::from(max_parallel));
    }
    if object.contains_key("worktreeRetention") {
        if config_patch_value_is_null(&object, "worktreeRetention")
            || typed
                .worktree_retention
                .is_some_and(|value| value.is_none())
        {
            raw.remove("worktreeRetention");
        } else if let Some(Some(retention)) = typed.worktree_retention {
            raw.insert("worktreeRetention".to_owned(), Value::from(retention));
        }
    }
    for (field, key) in [
        ("liveTitleUpdates", "liveTitleUpdates"),
        ("reviewGate", "reviewGate"),
    ] {
        if !object.contains_key(field) {
            continue;
        }
        if object.get(field).is_some_and(Value::is_null) {
            raw.remove(key);
        } else if let Some(value) = object.get(field).and_then(Value::as_bool) {
            raw.insert(key.to_owned(), Value::Bool(value));
        }
    }
    if object.contains_key("memoryLimitMb") {
        if config_patch_value_is_null(&object, "memoryLimitMb")
            || typed.memory_limit_mb.is_some_and(|value| value == Some(0))
        {
            raw.remove("memoryLimitMb");
        } else if let Some(Some(limit)) = typed.memory_limit_mb {
            raw.insert("memoryLimitMb".to_owned(), Value::from(limit));
        }
    }
    if typed.default_models.is_some() {
        let current_models = raw
            .get("defaultModels")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut models = current_models;
        if let Some(value) = object.get("defaultModels").and_then(Value::as_object) {
            for key in ["claude", "codex", "opencode", "pi"] {
                let Some(value) = value.get(key) else {
                    continue;
                };
                if value.is_null() || value.as_str().is_some_and(|model| model.trim().is_empty()) {
                    models.remove(key);
                } else if let Some(model) = value.as_str() {
                    models.insert(key.to_owned(), Value::String(model.trim().to_owned()));
                }
            }
        }
        if models.is_empty() {
            raw.remove("defaultModels");
        } else {
            raw.insert("defaultModels".to_owned(), Value::Object(models));
        }
    }
    if let Err(error) = atomic_write_json_sync(&repo_config_path(&state), &Value::Object(raw)) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    get_config(State(state)).await
}

async fn update_scoped_config(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(input): Json<Value>,
) -> Response {
    update_config(State(state), Json(input)).await
}

async fn list_agent_config(State(state): State<ServerState>) -> Response {
    Json(agent_config_listing(&state.config.repo_root)).into_response()
}

async fn list_scoped_agent_config(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    list_agent_config(State(state)).await
}

async fn get_agent_config(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(definition) = agent_config_definition(&id) else {
        return error_response(StatusCode::NOT_FOUND, "unknown config file");
    };
    match agent_config_content(definition, &state.config.repo_root) {
        Ok(content) => Json(content).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error),
    }
}

async fn get_scoped_agent_config(
    State(state): State<ServerState>,
    AxumPath((_project, id)): AxumPath<(String, String)>,
) -> Response {
    get_agent_config(State(state), AxumPath(id)).await
}

fn parse_agent_config_input(input: Value) -> Result<SetAgentConfigInput, String> {
    let Some(object) = input.as_object() else {
        return Err("invalid agent config body".to_owned());
    };
    if !object.contains_key("content") || !object.contains_key("version") {
        return Err("content and version are required".to_owned());
    }
    let parsed =
        serde_json::from_value::<SetAgentConfigInput>(input).map_err(|error| error.to_string())?;
    if parsed.content.chars().count() > 2_000_000 {
        return Err("content must be at most 2000000 characters".to_owned());
    }
    Ok(parsed)
}

fn write_agent_config(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
    input: SetAgentConfigInput,
) -> Result<AgentConfigFileContent, (StatusCode, String)> {
    if let Err(error) = validate_agent_config(&input.content, definition.format) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid {:?}: {error}", definition.format).to_lowercase(),
        ));
    }
    let path = resolve_agent_config_path(definition, repo_root);
    let current = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
    };
    if input.content.trim().is_empty()
        && current
            .as_ref()
            .is_some_and(|bytes| !String::from_utf8_lossy(bytes).trim().is_empty())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "refusing to overwrite a non-empty config file with empty content — delete the file manually if you mean to remove it"
                .to_owned(),
        ));
    }
    let current_version = current.as_deref().map(config_hash);
    if current_version != input.version {
        return Err((
            StatusCode::CONFLICT,
            if current_version.is_none() {
                "the file no longer exists on disk — reload before saving".to_owned()
            } else {
                "the file changed on disk since you opened it — reload before saving".to_owned()
            },
        ));
    }
    let target = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if let Some(parent) = target.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = PathBuf::from(format!(
        "{}.duck-tmp-{}-{nonce}",
        target.display(),
        std::process::id()
    ));
    if let Err(error) = fs::write(&temporary, input.content.as_bytes()) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
    }
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
    }
    match agent_config_content(definition, repo_root) {
        Ok(content) => Ok(content),
        Err(error) => Err((StatusCode::INTERNAL_SERVER_ERROR, error)),
    }
}

async fn update_agent_config(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<Value>,
) -> Response {
    let Some(definition) = agent_config_definition(&id) else {
        return error_response(StatusCode::NOT_FOUND, "unknown config file");
    };
    let input = match parse_agent_config_input(input) {
        Ok(input) => input,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error),
    };
    match write_agent_config(definition, &state.config.repo_root, input) {
        Ok(content) => Json(content).into_response(),
        Err((status, error)) => error_response(status, &error),
    }
}

async fn update_scoped_agent_config(
    State(state): State<ServerState>,
    AxumPath((_project, id)): AxumPath<(String, String)>,
    Json(input): Json<Value>,
) -> Response {
    update_agent_config(State(state), AxumPath(id), Json(input)).await
}

const IDE_FILE_MAX_BYTES: usize = 1_000_000;
const IDE_DIRECTORY_MAX_ENTRIES: usize = 2_000;

fn ide_display_path(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .ok()
        .map(|relative| {
            relative
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(value) => Some(value.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}

fn normalize_ide_path(root: &Path, path: &str) -> Result<PathBuf, (StatusCode, String)> {
    if path.chars().count() > 4_096
        || path.contains('\0')
        || path.contains('\\')
        || Path::new(path).is_absolute()
    {
        return Err((StatusCode::BAD_REQUEST, "invalid project path".to_owned()));
    }
    let mut target = root.to_path_buf();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(value) => target.push(value),
            std::path::Component::ParentDir => {
                if target == root || !target.pop() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "path is outside the project".to_owned(),
                    ));
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err((StatusCode::BAD_REQUEST, "invalid project path".to_owned()));
            }
        }
    }
    Ok(target)
}

fn resolve_ide_path(
    root: &Path,
    path: &str,
    directory: bool,
) -> Result<(PathBuf, PathBuf), (StatusCode, String)> {
    let project_root = fs::canonicalize(root)
        .map_err(|_| (StatusCode::NOT_FOUND, "project folder not found".to_owned()))?;
    let lexical = normalize_ide_path(&project_root, path)?;
    let target = fs::canonicalize(&lexical).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            if directory {
                "directory not found"
            } else {
                "file not found"
            }
            .to_owned(),
        )
    })?;
    if !target.starts_with(&project_root) {
        return Err((
            StatusCode::BAD_REQUEST,
            "path is outside the project".to_owned(),
        ));
    }
    if target != lexical {
        return Err((
            StatusCode::BAD_REQUEST,
            "symbolic links are not editable".to_owned(),
        ));
    }
    let metadata = fs::symlink_metadata(&target).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            if directory {
                "directory not found"
            } else {
                "file not found"
            }
            .to_owned(),
        )
    })?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err((
            StatusCode::NOT_FOUND,
            if directory {
                "directory not found"
            } else {
                "file not found"
            }
            .to_owned(),
        ));
    }
    Ok((project_root, target))
}

fn ide_list_directory(
    root: &Path,
    path: &str,
) -> Result<IdeDirectoryResponse, (StatusCode, String)> {
    let (project_root, target) = resolve_ide_path(root, path, true)?;
    let entries = fs::read_dir(&target).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            "directory is not readable".to_owned(),
        )
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                "directory is not readable".to_owned(),
            )
        })?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                "directory is not readable".to_owned(),
            )
        })?;
        if file_type.is_dir() || file_type.is_file() {
            candidates.push((
                name.to_string_lossy().into_owned(),
                entry.path(),
                file_type.is_dir(),
            ));
        }
    }
    candidates.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let truncated = candidates.len() > IDE_DIRECTORY_MAX_ENTRIES;
    let mut output = Vec::new();
    for (name, entry_path, is_directory) in candidates.into_iter().take(IDE_DIRECTORY_MAX_ENTRIES) {
        let path = ide_display_path(&project_root, &entry_path);
        if is_directory {
            output.push(IdeEntry {
                name,
                path,
                entry_type: IdeEntryType::Dir,
                size: None,
            });
        } else if let Ok(metadata) = fs::metadata(&entry_path)
            && metadata.is_file()
        {
            output.push(IdeEntry {
                name,
                path,
                entry_type: IdeEntryType::File,
                size: Some(metadata.len()),
            });
        }
    }
    Ok(IdeDirectoryResponse {
        path: if path.is_empty() {
            String::new()
        } else {
            ide_display_path(&project_root, &target)
        },
        entries: output,
        truncated,
    })
}

fn ide_read_file(root: &Path, path: &str) -> Result<IdeFileResponse, (StatusCode, String)> {
    if path.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".to_owned()));
    }
    let (project_root, target) = resolve_ide_path(root, path, false)?;
    let metadata =
        fs::metadata(&target).map_err(|_| (StatusCode::NOT_FOUND, "file not found".to_owned()))?;
    if metadata.len() > IDE_FILE_MAX_BYTES as u64 {
        return Err((StatusCode::CONFLICT, "file is too large to edit".to_owned()));
    }
    let bytes = fs::read(&target)
        .map_err(|_| (StatusCode::NOT_FOUND, "file is not readable".to_owned()))?;
    if bytes.contains(&0) {
        return Err((
            StatusCode::CONFLICT,
            "binary files cannot be edited".to_owned(),
        ));
    }
    let content = String::from_utf8(bytes.clone()).map_err(|_| {
        (
            StatusCode::CONFLICT,
            "binary files cannot be edited".to_owned(),
        )
    })?;
    Ok(IdeFileResponse {
        path: ide_display_path(&project_root, &target),
        content,
        size: bytes.len() as u64,
    })
}

fn ide_write_file(
    root: &Path,
    input: IdeFileInput,
) -> Result<IdeFileResponse, (StatusCode, String)> {
    if input.path.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".to_owned()));
    }
    if input.content.len() > IDE_FILE_MAX_BYTES {
        return Err((StatusCode::CONFLICT, "file is too large to edit".to_owned()));
    }
    let (_, target) = resolve_ide_path(root, &input.path, false)?;
    fs::write(&target, input.content.as_bytes())
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
    ide_read_file(root, &input.path)
}

async fn list_ide_directory(
    State(state): State<ServerState>,
    Query(query): Query<IdeDirectoryQuery>,
) -> Response {
    match ide_list_directory(
        &state.config.repo_root,
        query.path.as_deref().unwrap_or_default(),
    ) {
        Ok(body) => Json(body).into_response(),
        Err((status, error)) => error_response(status, &error),
    }
}

async fn list_scoped_ide_directory(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Query(query): Query<IdeDirectoryQuery>,
) -> Response {
    list_ide_directory(State(state), Query(query)).await
}

async fn read_ide_file(
    State(state): State<ServerState>,
    Query(query): Query<IdeFileQuery>,
) -> Response {
    match ide_read_file(&state.config.repo_root, &query.path) {
        Ok(body) => Json(body).into_response(),
        Err((status, error)) => error_response(status, &error),
    }
}

async fn read_scoped_ide_file(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Query(query): Query<IdeFileQuery>,
) -> Response {
    read_ide_file(State(state), Query(query)).await
}

async fn write_ide_file(
    State(state): State<ServerState>,
    Json(input): Json<IdeFileInput>,
) -> Response {
    match ide_write_file(&state.config.repo_root, input) {
        Ok(body) => Json(body).into_response(),
        Err((status, error)) => error_response(status, &error),
    }
}

async fn write_scoped_ide_file(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(input): Json<IdeFileInput>,
) -> Response {
    write_ide_file(State(state), Json(input)).await
}

async fn get_ui_state(State(state): State<ServerState>) -> Response {
    Json(Value::Object(read_repo_ui_state(&state))).into_response()
}

async fn get_scoped_ui_state(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    get_ui_state(State(state)).await
}

async fn update_ui_state(State(state): State<ServerState>, Json(input): Json<Value>) -> Response {
    match merge_repo_ui_state(&state, input) {
        Ok(merged) => Json(Value::Object(merged)).into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, &error),
    }
}

async fn update_scoped_ui_state(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(input): Json<Value>,
) -> Response {
    update_ui_state(State(state), Json(input)).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTodoInput {
    runner: Option<RunnerSelection>,
    model: Option<String>,
    prompt: Option<String>,
}

fn todo_workflow(state: &ServerState, todo: &TodoItem) -> coducktor_contract::WorkflowDef {
    if let Some(skill_name) = todo.suggested_skill.as_deref() {
        let skills = discover_skills(&workflow_repo_root(state), &ProcessEnv);
        if skills.iter().any(|skill| skill.name == skill_name) {
            return coducktor_contract::WorkflowDef {
                name: "(inbox)".to_owned(),
                description: Some(format!("Follow-up from the inbox — skill \"{skill_name}\"")),
                steps: vec![WorkflowStepDef {
                    id: "task".to_owned(),
                    name: Some("Do the task".to_owned()),
                    prompt: Some("{{task}}".to_owned()),
                    skill: Some(skill_name.to_owned()),
                    model: None,
                    runner: None,
                    allowed_tools: None,
                    bash_allowlist: None,
                    command: None,
                    on_fail: None,
                }],
                source: coducktor_contract::WorkflowSource::BuiltIn,
                path: None,
            };
        }
    }
    quick_task_workflow()
}

async fn list_todos(State(state): State<ServerState>) -> Response {
    if !followups_enabled(&ProcessEnv) {
        return Json(Vec::<TodoItem>::new()).into_response();
    }
    Json(coducktor_core::todos::read_todos(&repo_data_dir(&state))).into_response()
}

async fn list_scoped_todos(
    State(state): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Response {
    list_todos(State(state)).await
}

async fn delete_todo(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    if !followups_enabled(&ProcessEnv) {
        return error_response(
            StatusCode::CONFLICT,
            "the follow-up inbox is disabled — set CEZ_FOLLOWUPS=1 to enable it",
        );
    }
    match coducktor_core::todos::remove_todo(&repo_data_dir(&state), &id) {
        Ok(true) => Json(coducktor_contract::RemoveTodoResponse { removed: true }).into_response(),
        Ok(false) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn delete_scoped_todo(
    State(state): State<ServerState>,
    AxumPath((_project, id)): AxumPath<(String, String)>,
) -> Response {
    delete_todo(State(state), AxumPath(id)).await
}

async fn start_todo(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<StartTodoInput>>,
) -> Response {
    if !followups_enabled(&ProcessEnv) {
        return error_response(
            StatusCode::CONFLICT,
            "the follow-up inbox is disabled — set CEZ_FOLLOWUPS=1 to enable it",
        );
    }
    let data_dir = repo_data_dir(&state);
    let Some(todo) = coducktor_core::todos::read_todos(&data_dir)
        .into_iter()
        .find(|todo| todo.id == id)
    else {
        return error_response(StatusCode::NOT_FOUND, "not found");
    };
    if todo.started_task_id.is_some() {
        return error_response(StatusCode::CONFLICT, "already started");
    }
    let input = body.map(|Json(input)| input).unwrap_or_default();
    if input
        .model
        .as_ref()
        .is_some_and(|model| model.chars().count() > 200)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "model must be at most 200 characters",
        );
    }
    let mut task = coducktor_core::todos::todo_task_text(
        &todo.summary,
        todo.suggested_prompt.as_deref(),
        todo.suggested_args.as_deref(),
    );
    if let Some(prompt) = input
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        if prompt.chars().count() > 20_000 {
            return error_response(
                StatusCode::BAD_REQUEST,
                "prompt must be at most 20000 characters",
            );
        }
        task.push_str("\n\n");
        task.push_str(prompt);
    }
    let workflow = todo_workflow(&state, &todo);
    let run = {
        let Ok(mut manager) = state.manager.lock() else {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
        };
        match manager.start_run(
            &workflow,
            CoreStartRunInput {
                task,
                runner: input.runner,
                model: input.model,
                ..CoreStartRunInput::default()
            },
        ) {
            Ok(run) => run,
            Err(error) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        }
    };
    match coducktor_core::todos::mark_started(&data_dir, &id, &run.id) {
        Ok(true) => (StatusCode::CREATED, Json(StartTodoResponse { run })).into_response(),
        Ok(false) => error_response(StatusCode::CONFLICT, "already started"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn start_scoped_todo(
    State(state): State<ServerState>,
    AxumPath((_project, id)): AxumPath<(String, String)>,
    body: Option<Json<StartTodoInput>>,
) -> Response {
    start_todo(State(state), AxumPath(id), body).await
}

async fn get_workspace_usage() -> Response {
    Json(coducktor_contract::WorkspaceUsageResponse { providers: vec![] }).into_response()
}

async fn list_runs(State(state): State<ServerState>) -> Response {
    let Ok(manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let runs = manager
        .list_runs()
        .into_iter()
        .map(api_run)
        .collect::<Vec<_>>();
    Json(runs).into_response()
}

async fn get_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.get_run(&id).cloned() {
        Some(run) => Json(api_run(run)).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn create_run(
    State(state): State<ServerState>,
    Json(input): Json<CreateRunInput>,
) -> Response {
    let workflow = match workflow_for_input(state.config(), &input) {
        Ok(workflow) => workflow,
        Err((status, message)) => return error_response(status, &message),
    };
    let core_input = CoreStartRunInput {
        task: input.task,
        model: input.model,
        reasoning_effort: input.reasoning_effort,
        runner: input.runner,
        agent_profile: input.agent_profile,
        system_prompt: input.system_prompt,
        generate_followups: input.generate_followups,
        autonomous: input.autonomous,
        worktree: input.worktree,
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let variants = input.variants.unwrap_or(1.0);
    if !variants.is_finite() || variants.fract() != 0.0 || !(1.0..=3.0).contains(&variants) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "variants must be an integer from 1 to 3",
        );
    }
    if variants > 1.0 {
        return match manager.start_variants(&workflow, core_input, variants as usize) {
            Ok(runs) => {
                (StatusCode::CREATED, Json(CreateRunResponse::Group { runs })).into_response()
            }
            Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        };
    }
    match manager.start_run(&workflow, core_input) {
        Ok(run) => (
            StatusCode::CREATED,
            Json(CreateRunResponse::Single(Box::new(run))),
        )
            .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn archive_finished(State(state): State<ServerState>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.archive_finished() {
        Ok(archived) => Json(coducktor_contract::ArchiveFinishedResponse {
            archived: archived as f64,
        })
        .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn mark_all_read(State(state): State<ServerState>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.mark_all_read() {
        Ok(read) => Json(MarkAllReadResponse { read: read as f64 }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ArchiveInput {
    archived: Option<bool>,
}

async fn archive_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<ArchiveInput>>,
) -> Response {
    let archived = body.and_then(|Json(value)| value.archived).unwrap_or(true);
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    match manager.archive(&id, archived) {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn read_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    mutate_read(state, id, true)
}

async fn unread_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    mutate_read(state, id, false)
}

fn mutate_read(state: ServerState, id: String, read: bool) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let result = if read {
        manager.mark_read(&id)
    } else {
        manager.mark_unread(&id)
    };
    match result {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn patch_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<PatchRunInput>,
) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    let Some(current) = manager.get_run(&id).cloned() else {
        return error_response(StatusCode::NOT_FOUND, "not found");
    };
    if input.task.is_some() && current.status != coducktor_contract::RunStatus::Queued {
        return error_response(StatusCode::CONFLICT, "run already started");
    }
    let mut value = Map::new();
    if let Some(title) = input.title {
        value.insert("title".to_owned(), Value::String(title.clone()));
        value.insert("titleSummary".to_owned(), Value::String(title));
        value.insert("titleOrigin".to_owned(), Value::String("user".to_owned()));
    }
    if let Some(task) = input.task {
        value.insert("task".to_owned(), Value::String(task));
    }
    match manager.update_run_value(&id, Value::Object(value)) {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn cancel_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.cancel(&id) {
        Ok(cancelled) => Json(coducktor_contract::CancelResponse { cancelled }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn finish_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.finish(&id) {
        Ok(true) => Json(coducktor_contract::FinishResponse { finished: true }).into_response(),
        Ok(false) => error_response(StatusCode::CONFLICT, "no open session"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn continue_run(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<ContinueInput>>,
) -> Response {
    let input = body.map(|Json(value)| value).unwrap_or_default();
    let options = ContinueOptions {
        text: input.text,
        runner: input.runner,
        model: input.model,
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.continue_run(&id, options) {
        Ok(result) if result.ok => {
            Json(coducktor_contract::ContinueResponse { continued: true }).into_response()
        }
        Ok(result) => error_response(
            StatusCode::CONFLICT,
            result.error.as_deref().unwrap_or("cannot continue run"),
        ),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn send_message(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<MessageInput>,
) -> Response {
    let Some(text) = input.text.filter(|value| !value.trim().is_empty()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "message needs text or at least one image",
        );
    };
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    match manager.send_message(&id, text) {
        Ok(true) => {
            Json(coducktor_contract::MessageResponse::Delivered { delivered: true }).into_response()
        }
        Ok(false) => error_response(StatusCode::CONFLICT, "session closed"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn cancel_auto_resume(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    let mut patch = Map::new();
    patch.insert("autoResumeAt".to_owned(), Value::Null);
    patch.insert("autoResumeAttempts".to_owned(), Value::Null);
    match manager.update_run_value(&id, Value::Object(patch)) {
        Ok(Some(_)) => {
            Json(coducktor_contract::CancelAutoResumeResponse { cancelled: true }).into_response()
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn delete_run(State(state): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(mut manager) = state.manager.lock() else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "run manager unavailable");
    };
    if manager.get_run(&id).is_none() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }
    if manager.is_active(&id) {
        return error_response(StatusCode::CONFLICT, "cannot delete an active run");
    }
    match manager.remove_run(&id) {
        Ok(deleted) => Json(DeleteRunResponse { deleted }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

fn workflow_for_input(
    config: &ServerConfig,
    input: &CreateRunInput,
) -> Result<coducktor_contract::WorkflowDef, (StatusCode, String)> {
    if let Some(steps) = &input.steps {
        if steps.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "steps must not be empty".to_owned(),
            ));
        }
        return Ok(coducktor_contract::WorkflowDef {
            name: "(planned)".to_owned(),
            description: None,
            steps: steps.clone(),
            source: coducktor_contract::WorkflowSource::BuiltIn,
            path: None,
        });
    }
    let Some(name) = input.workflow.as_deref() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "workflow or steps is required".to_owned(),
        ));
    };
    load_workflows(&config.repo_root)
        .0
        .into_iter()
        .find(|workflow| workflow.name == name)
        .ok_or((StatusCode::NOT_FOUND, format!("unknown workflow: {name}")))
}

fn api_run(record: RunRecord) -> ApiRun {
    ApiRun {
        record,
        usage: None,
    }
}

fn health_payload(config: &ServerConfig) -> HealthResponse {
    let repo_root = config.repo_root.to_string_lossy().into_owned();
    let branch = git_output(&config.repo_root, &["branch", "--show-current"]);
    let remote = git_output(&config.repo_root, &["config", "--get", "remote.origin.url"]);
    let repo = if branch.is_some() || remote.is_some() {
        Some(RepoInfo {
            root: repo_root.clone(),
            branch: branch.unwrap_or_default(),
            remote,
        })
    } else {
        None
    };

    HealthResponse {
        version: config.version.clone(),
        latest_version: None,
        repo_root,
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
        .map(|(name, binary)| backend_check(name, binary))
        .collect(),
        default_runner: RunnerSelection::Auto,
        forge: Some(ForgeInfo {
            kind: ForgeKind::GitHub,
            available: None,
            reason: Some("Rust server forge routes are not installed yet".to_owned()),
        }),
        capabilities: Capabilities {
            followups: std::env::var("DUCK_FOLLOWUPS")
                .or_else(|_| std::env::var("CEZ_FOLLOWUPS"))
                .is_ok_and(|value| value == "1"),
        },
        projects: vec![HealthProject {
            id: "default".to_owned(),
            name: config
                .repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project")
                .to_owned(),
        }],
        boot_project: "default".to_owned(),
    }
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
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

async fn request_origin_guard(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !request.uri().path().starts_with("/api/") {
        return next.run(request).await;
    }

    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok());
    let host_name = host.and_then(hostname);
    if !host_name.is_some_and(is_loopback_host) {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden: unexpected Host header — this request did not originate from this machine",
        );
    }

    if request.uri().path() == HEALTH_PATH {
        return next.run(request).await;
    }

    if let Some(project) = scoped_project(request.uri().path())
        && !project_alias_allowed(state.config(), project)
    {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }

    if is_mutating(request.method()) {
        if let Some(origin) = request
            .headers()
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            let same_origin = authority(origin)
                .zip(host)
                .is_some_and(|(origin, host)| origin == host);
            let is_dev_proxy = request
                .headers()
                .get(SEC_FETCH_SITE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == "same-origin")
                && origin
                    .strip_prefix("http://")
                    .or_else(|| origin.strip_prefix("https://"))
                    .and_then(hostname)
                    .is_some_and(is_loopback_host);
            if !same_origin && !is_dev_proxy {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "forbidden: cross-origin request rejected (same-origin only)",
                );
            }
        }
        if request
            .headers()
            .get(SEC_FETCH_SITE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "cross-site")
        {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden: cross-site request rejected (same-origin only)",
            );
        }
    }

    next.run(request).await
}

fn scoped_project(path: &str) -> Option<&str> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    if segments.next() != Some("api")
        || segments.next() != Some("v1")
        || segments.next() != Some("p")
    {
        return None;
    }
    segments.next()
}

fn project_alias_allowed(config: &ServerConfig, project: &str) -> bool {
    project == "default"
        || config
            .repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == project)
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn hostname(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split(']').next();
    }
    value.rsplit_once(':').map_or(Some(value), |(host, port)| {
        port.parse::<u16>().ok().map(|_| host)
    })
}

fn authority(value: &str) -> Option<&str> {
    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))?;
    Some(
        without_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default(),
    )
}

fn is_loopback_host(value: &str) -> bool {
    if value.eq_ignore_ascii_case("localhost") {
        return true;
    }
    value
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn error_response(status: StatusCode, error: &str) -> Response {
    #[derive(Serialize)]
    struct ErrorBody<'a> {
        error: &'a str,
    }
    (status, Json(ErrorBody { error })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use coducktor_core::workflows::run::{CreateRunInput as CoreCreateRunInput, RunManager};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static TEST_REPO_ID: AtomicU64 = AtomicU64::new(0);

    fn test_router() -> Router {
        router(ServerConfig::new("/tmp/not-a-repo", "test"))
    }

    fn test_repo() -> std::path::PathBuf {
        let id = TEST_REPO_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("coducktor-server-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("test repo directory");
        path
    }

    fn seeded_router() -> (Router, std::path::PathBuf, String) {
        let repo = test_repo();
        let data_dir = repo.join(".ai").join("coducktor");
        let mut manager = RunManager::open(&data_dir);
        let run = manager
            .create_run(CoreCreateRunInput {
                title: "original".to_owned(),
                workflow: "manual".to_owned(),
                task: "queued task".to_owned(),
                ..CoreCreateRunInput::default()
            })
            .expect("seed run");
        let state = ServerState::with_manager(ServerConfig::new(&repo, "test"), manager);
        (router_with_state(state), repo, run.id)
    }

    async fn send(router: &Router, method: Method, uri: &str, body: Option<Value>) -> Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "127.0.0.1:4321");
        let request_body = match body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                Body::from(serde_json::to_vec(&body).expect("request JSON"))
            }
            None => Body::empty(),
        };
        router
            .clone()
            .oneshot(builder.body(request_body).expect("test request"))
            .await
            .expect("router response")
    }

    async fn json_body(response: Response) -> Value {
        let bytes = response_body(response).await;
        serde_json::from_slice(&bytes).expect("response JSON")
    }

    async fn response_body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec()
    }

    #[tokio::test]
    async fn health_is_versioned_and_cors_readable() {
        let request = Request::builder()
            .uri(HEALTH_PATH)
            .header(HOST, "127.0.0.1:4321")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("*"))
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let parsed: HealthResponse = serde_json::from_slice(&body).expect("health JSON");
        assert_eq!(parsed.version, "test");
        assert_eq!(parsed.boot_project, "default");
    }

    #[tokio::test]
    async fn non_loopback_hosts_are_rejected_before_route_dispatch() {
        let request = Request::builder()
            .uri(HEALTH_PATH)
            .header(HOST, "127.0.0.1.evil.example:4321")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cross_origin_writes_are_rejected() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/runs")
            .header(HOST, "127.0.0.1:4321")
            .header(ORIGIN, "http://127.0.0.1:9999")
            .body(Body::empty())
            .expect("test request is valid");
        let response = test_router()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn runs_routes_cover_create_list_patch_receipts_archive_and_delete() {
        let (router, repo, run_id) = seeded_router();

        let response = send(&router, Method::GET, "/api/v1/runs", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let runs = json_body(response).await;
        assert_eq!(runs.as_array().map(Vec::len), Some(1));
        assert_eq!(runs[0]["id"], run_id);

        let response = send(
            &router,
            Method::PATCH,
            &format!("/api/v1/runs/{run_id}"),
            Some(serde_json::json!({ "title": "renamed", "task": "edited task" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let patched = json_body(response).await;
        assert_eq!(patched["title"], "renamed");
        assert_eq!(patched["titleSummary"], "renamed");
        assert_eq!(patched["titleOrigin"], "user");
        assert_eq!(patched["task"], "edited task");

        let response = send(
            &router,
            Method::POST,
            &format!("/api/v1/runs/{run_id}/read"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(json_body(response).await["seenAt"].is_string());

        let response = send(
            &router,
            Method::POST,
            &format!("/api/v1/runs/{run_id}/archive"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["archived"], true);

        let response = send(
            &router,
            Method::DELETE,
            &format!("/api/v1/runs/{run_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({ "deleted": true })
        );

        let response = send(
            &router,
            Method::GET,
            &format!("/api/v1/runs/{run_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["error"], "not found");
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn runs_routes_keep_default_and_boot_scope_aliases_byte_identical() {
        let (router, repo, _) = seeded_router();
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");
        let mut bodies = Vec::new();
        for path in [
            "/api/v1/runs".to_owned(),
            "/api/v1/p/default/runs".to_owned(),
            format!("/api/v1/p/{boot}/runs"),
        ] {
            let response = send(&router, Method::GET, &path, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            bodies.push(response_body(response).await);
        }
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[0], bodies[2]);

        let response = send(&router, Method::GET, "/api/v1/p/no-such-project/runs", None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn workspace_routes_persist_projects_config_and_ui_state() {
        let repo = test_repo();
        let workspace = repo.join("workspace");
        let manager = RunManager::open(repo.join(".ai").join("coducktor"));
        let router = router_with_state(ServerState::with_manager_and_workspace_dir(
            ServerConfig::new(&repo, "test"),
            manager,
            &workspace,
        ));

        let response = send(&router, Method::GET, "/api/v1/projects", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let projects = json_body(response).await;
        assert_eq!(projects["projects"].as_array().map(Vec::len), Some(0));
        let boot_project = projects["bootProject"].as_str().expect("boot project");

        let child = repo.join("child");
        fs::create_dir_all(&child).expect("child project directory");
        let response = send(
            &router,
            Method::POST,
            "/api/v1/projects",
            Some(serde_json::json!({ "root": child })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let registered = json_body(response).await;
        let child_id = registered["project"]["id"]
            .as_str()
            .expect("child project id")
            .to_owned();

        let response = send(
            &router,
            Method::PATCH,
            &format!("/api/v1/projects/{child_id}"),
            Some(serde_json::json!({ "maxParallel": 4, "tags": [" Storefront ", "storefront"] })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated = json_body(response).await;
        assert_eq!(updated["project"]["maxParallel"].as_f64(), Some(4.0));
        assert_eq!(
            updated["project"]["tags"],
            serde_json::json!(["Storefront"])
        );

        let response = send(&router, Method::GET, "/api/v1/workspace/config", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["resources"]["maxParallel"], 2);

        let response = send(
            &router,
            Method::PUT,
            "/api/v1/workspace/config",
            Some(serde_json::json!({ "resources": { "maxParallel": 4 } })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["resources"]["maxParallel"], 4);

        let response = send(
            &router,
            Method::PUT,
            "/api/v1/workspace/ui-state",
            Some(serde_json::json!({
                "appearance": { "density": "compact" },
                "futurePreference": { "enabled": true }
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let ui_state = json_body(response).await;
        assert_eq!(ui_state["appearance"]["density"], "compact");
        assert_eq!(ui_state["futurePreference"]["enabled"], true);

        let response = send(
            &router,
            Method::DELETE,
            &format!("/api/v1/projects/{child_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({ "removed": true, "id": child_id })
        );

        let response = send(
            &router,
            Method::DELETE,
            &format!("/api/v1/projects/{boot_project}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn skills_and_workflows_routes_share_project_scope_aliases() {
        let repo = test_repo();
        let skills_dir = repo.join(".ai").join("skills");
        fs::create_dir_all(&skills_dir).expect("skills directory");
        fs::write(
            skills_dir.join("guide.md"),
            "---\nname: guide\ndescription: A guide\ninteractive: true\n---\nUse the guide.\n",
        )
        .expect("skill file");
        let manager = RunManager::open(repo.join(".ai").join("coducktor"));
        let router = router_with_state(ServerState::with_manager(
            ServerConfig::new(&repo, "test"),
            manager,
        ));
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");

        let skills = send(&router, Method::GET, "/api/v1/skills", None).await;
        assert_eq!(skills.status(), StatusCode::OK);
        let scoped_skills = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/skills"),
            None,
        )
        .await;
        assert_eq!(scoped_skills.status(), StatusCode::OK);
        assert_eq!(
            response_body(skills).await,
            response_body(scoped_skills).await
        );

        let workflows = send(&router, Method::GET, "/api/v1/workflows", None).await;
        assert_eq!(workflows.status(), StatusCode::OK);
        assert_eq!(
            json_body(workflows).await["workflows"][0]["name"],
            "quick-task"
        );

        let response = send(
            &router,
            Method::POST,
            "/api/v1/workflows",
            Some(serde_json::json!({
                "name": "Review",
                "steps": [{ "id": "work", "prompt": "{{task}}" }]
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(json_body(response).await["name"], "Review");

        let scoped_workflows = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/workflows"),
            None,
        )
        .await;
        assert_eq!(scoped_workflows.status(), StatusCode::OK);
        assert!(
            json_body(scoped_workflows).await["workflows"]
                .as_array()
                .is_some_and(|workflows| workflows
                    .iter()
                    .any(|workflow| workflow["name"] == "Review"))
        );

        let parsed = send(
            &router,
            Method::POST,
            "/api/v1/workflows/parse",
            Some(serde_json::json!({
                "yaml": "name: parsed\nskills:\n  - guide\n"
            })),
        )
        .await;
        assert_eq!(parsed.status(), StatusCode::OK);
        assert_eq!(json_body(parsed).await["steps"][0]["skill"], "guide");

        let deleted = send(&router, Method::DELETE, "/api/v1/workflows/Review", None).await;
        assert_eq!(deleted.status(), StatusCode::OK);
        assert_eq!(json_body(deleted).await["ok"], true);

        let missing = send(&router, Method::DELETE, "/api/v1/workflows/Review", None).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn ui_state_and_todos_routes_keep_project_aliases_and_gates() {
        let repo = test_repo();
        let data_dir = repo.join(".ai").join("coducktor");
        fs::create_dir_all(&data_dir).expect("data directory");
        fs::write(
            data_dir.join("todos.json"),
            serde_json::json!([{ "id": "todo-1", "summary": "follow up" }]).to_string(),
        )
        .expect("todos file");
        let manager = RunManager::open(&data_dir);
        let router = router_with_state(ServerState::with_manager(
            ServerConfig::new(&repo, "test"),
            manager,
        ));
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");

        for path in [
            "/api/v1/ui-state".to_owned(),
            "/api/v1/p/default/ui-state".to_owned(),
            format!("/api/v1/p/{boot}/ui-state"),
        ] {
            let response = send(&router, Method::GET, &path, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json_body(response).await, serde_json::json!({}));
        }

        let updated = send(
            &router,
            Method::PUT,
            "/api/v1/ui-state",
            Some(serde_json::json!({
                "appearance": { "density": "compact" },
                "futurePreference": { "enabled": true }
            })),
        )
        .await;
        assert_eq!(updated.status(), StatusCode::OK);
        assert_eq!(
            json_body(updated).await["futurePreference"]["enabled"],
            true
        );

        let scoped = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/ui-state"),
            None,
        )
        .await;
        assert_eq!(scoped.status(), StatusCode::OK);
        assert_eq!(json_body(scoped).await["appearance"]["density"], "compact");

        let invalid = send(
            &router,
            Method::PUT,
            "/api/v1/ui-state",
            Some(serde_json::json!([])),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let todos = send(&router, Method::GET, "/api/v1/todos", None).await;
        assert_eq!(todos.status(), StatusCode::OK);
        assert_eq!(json_body(todos).await, serde_json::json!([]));
        let scoped_todos = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/todos"),
            None,
        )
        .await;
        assert_eq!(scoped_todos.status(), StatusCode::OK);
        assert_eq!(json_body(scoped_todos).await, serde_json::json!([]));
        let delete = send(&router, Method::DELETE, "/api/v1/todos/todo-1", None).await;
        assert_eq!(delete.status(), StatusCode::CONFLICT);
        assert!(
            fs::read_to_string(data_dir.join("todos.json"))
                .expect("todos file remains")
                .contains("todo-1")
        );
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn config_routes_merge_raw_values_and_keep_project_aliases() {
        let repo = test_repo();
        let workspace = repo.join("workspace");
        let manager = RunManager::open(repo.join(".ai").join("coducktor"));
        let router = router_with_state(ServerState::with_manager_and_workspace_dir(
            ServerConfig::new(&repo, "test"),
            manager,
            &workspace,
        ));
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");

        let initial = send(&router, Method::GET, "/api/v1/config", None).await;
        assert_eq!(initial.status(), StatusCode::OK);
        let initial_body = json_body(initial).await;
        assert_eq!(initial_body["defaultRunner"], "claude");
        assert_eq!(initial_body["maxParallel"], 2);
        assert_eq!(initial_body["defaultModels"], serde_json::json!({}));

        let updated = send(
            &router,
            Method::PUT,
            "/api/v1/config",
            Some(serde_json::json!({
                "systemPrompt": "  Be brief.  ",
                "defaultModels": { "claude": "  opus  " },
                "maxParallel": 4,
                "worktreeRetention": 0,
                "memoryLimitMb": 0
            })),
        )
        .await;
        assert_eq!(updated.status(), StatusCode::OK);
        let updated_body = json_body(updated).await;
        assert_eq!(updated_body["systemPrompt"], "Be brief.");
        assert_eq!(updated_body["defaultModels"]["claude"], "opus");
        assert_eq!(updated_body["maxParallel"], 4);
        assert_eq!(updated_body["worktreeRetention"], 0);
        assert_eq!(updated_body["memoryLimitMb"], Value::Null);

        let merged = send(
            &router,
            Method::PUT,
            "/api/v1/config",
            Some(serde_json::json!({ "defaultModels": { "codex": "gpt" } })),
        )
        .await;
        assert_eq!(merged.status(), StatusCode::OK);
        assert_eq!(
            json_body(merged).await["defaultModels"],
            serde_json::json!({ "claude": "opus", "codex": "gpt" })
        );

        let cleared = send(
            &router,
            Method::PUT,
            "/api/v1/config",
            Some(serde_json::json!({ "defaultModels": { "claude": null } })),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(json_body(cleared).await["defaultModels"]["codex"], "gpt");

        let scoped = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/config"),
            None,
        )
        .await;
        assert_eq!(scoped.status(), StatusCode::OK);
        assert_eq!(json_body(scoped).await["defaultModels"]["codex"], "gpt");

        let invalid = send(
            &router,
            Method::PUT,
            "/api/v1/config",
            Some(serde_json::json!({ "defaultModels": null })),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn agent_config_routes_use_catalog_ids_and_stale_write_tokens() {
        let repo = test_repo();
        let data_dir = repo.join(".ai").join("coducktor");
        let manager = RunManager::open(&data_dir);
        let router = router_with_state(ServerState::with_manager(
            ServerConfig::new(&repo, "test"),
            manager,
        ));
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");

        let listing = send(&router, Method::GET, "/api/v1/agent-config", None).await;
        assert_eq!(listing.status(), StatusCode::OK);
        let listing_body = json_body(listing).await;
        assert_eq!(listing_body["editable"], true);
        assert_eq!(listing_body["files"].as_array().map(Vec::len), Some(14));
        assert!(listing_body["userMcp"].is_object());

        let absent = send(
            &router,
            Method::GET,
            "/api/v1/agent-config/claude.project.settings",
            None,
        )
        .await;
        assert_eq!(absent.status(), StatusCode::OK);
        assert_eq!(json_body(absent).await["version"], Value::Null);

        let created = send(
            &router,
            Method::PUT,
            "/api/v1/agent-config/claude.project.settings",
            Some(serde_json::json!({ "content": "{\"a\":1}", "version": null })),
        )
        .await;
        assert_eq!(created.status(), StatusCode::OK);
        let created_body = json_body(created).await;
        let version = created_body["version"].as_str().expect("version");
        assert_eq!(
            fs::read_to_string(repo.join(".claude/settings.json")).expect("created config"),
            "{\"a\":1}"
        );

        let scoped = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/agent-config/claude.project.settings"),
            None,
        )
        .await;
        assert_eq!(scoped.status(), StatusCode::OK);
        assert_eq!(json_body(scoped).await["version"], version);

        let stale = send(
            &router,
            Method::PUT,
            "/api/v1/agent-config/claude.project.settings",
            Some(serde_json::json!({ "content": "{\"a\":2}", "version": null })),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);

        let invalid = send(
            &router,
            Method::PUT,
            "/api/v1/agent-config/claude.project.settings",
            Some(serde_json::json!({ "content": "{bad", "version": version })),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let unknown = send(&router, Method::GET, "/api/v1/agent-config/nope", None).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn ide_routes_list_read_write_and_reject_escape_paths() {
        let repo = test_repo();
        fs::create_dir_all(repo.join("src")).expect("source directory");
        fs::create_dir_all(repo.join(".git")).expect("git directory");
        fs::write(repo.join("src/index.ts"), "export const answer = 42\n").expect("source file");
        fs::write(repo.join("secret.txt"), "secret").expect("secret file");
        let manager = RunManager::open(repo.join(".ai").join("coducktor"));
        let router = router_with_state(ServerState::with_manager(
            ServerConfig::new(&repo, "test"),
            manager,
        ));
        let boot = repo
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test repo name");

        let tree = send(&router, Method::GET, "/api/v1/ide/tree", None).await;
        assert_eq!(tree.status(), StatusCode::OK);
        let tree_body = json_body(tree).await;
        assert_eq!(tree_body["path"], "");
        assert!(
            tree_body["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry["name"] == "src" && entry["path"] == "src" && entry["type"] == "dir"
                }))
        );
        assert!(
            !tree_body["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == ".git"))
        );

        let read = send(
            &router,
            Method::GET,
            &format!("/api/v1/p/{boot}/ide/file?path=src%2Findex.ts"),
            None,
        )
        .await;
        assert_eq!(read.status(), StatusCode::OK);
        assert_eq!(
            json_body(read).await["content"],
            "export const answer = 42\n"
        );

        let write = send(
            &router,
            Method::PUT,
            "/api/v1/ide/file",
            Some(serde_json::json!({
                "path": "src/index.ts",
                "content": "export const answer = 43\n"
            })),
        )
        .await;
        assert_eq!(write.status(), StatusCode::OK);
        assert_eq!(
            json_body(write).await["content"],
            "export const answer = 43\n"
        );

        let traversal = send(
            &router,
            Method::GET,
            "/api/v1/ide/file?path=..%2Fsecret.txt",
            None,
        )
        .await;
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);

        fs::write(repo.join("binary.bin"), [0_u8, 1, 2, 3]).expect("binary file");
        let binary = send(
            &router,
            Method::GET,
            "/api/v1/ide/file?path=binary.bin",
            None,
        )
        .await;
        assert_eq!(binary.status(), StatusCode::CONFLICT);
        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn runs_routes_validate_workflow_selection_and_create_response() {
        let (router, repo, _) = seeded_router();

        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({ "task": "missing workflow" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await["error"],
            "workflow or steps is required"
        );

        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({
                "task": "run inline",
                "steps": [{ "id": "work", "prompt": "{{task}}" }]
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = json_body(response).await;
        assert!(created["id"].is_string());
        assert_eq!(created["task"], "run inline");

        let response = send(&router, Method::POST, "/api/v1/runs/read-all", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("content-type").is_some());
        assert!(json_body(response).await["read"].is_number());

        fs::remove_dir_all(repo).expect("remove test repo");
    }

    #[tokio::test]
    async fn task_patch_is_rejected_after_a_run_leaves_the_queue() {
        let (router, repo, _) = seeded_router();
        let response = send(
            &router,
            Method::POST,
            "/api/v1/runs",
            Some(serde_json::json!({
                "task": "starts and fails without a session factory",
                "steps": [{ "id": "work", "prompt": "{{task}}" }]
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let run_id = json_body(response).await["id"]
            .as_str()
            .expect("created run id")
            .to_owned();

        let response = send(
            &router,
            Method::PATCH,
            &format!("/api/v1/runs/{run_id}"),
            Some(serde_json::json!({ "task": "too late" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(json_body(response).await["error"], "run already started");
        fs::remove_dir_all(repo).expect("remove test repo");
    }
}
