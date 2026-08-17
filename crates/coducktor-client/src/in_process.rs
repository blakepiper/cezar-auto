//! `InProcessEngine` — an `Engine` implementation that calls straight into `coducktor-core`
//! (and, for the families that need them, `coducktor-runners`/`coducktor-forge`) instead of
//! making an HTTP request. Spec §12/plan C1: because the `Engine` trait predates the server
//! (A2), this is meant to be "an implementation, not an extraction" — in practice a large
//! fraction of `coducktor-server`'s own handlers turned out to hold real business logic
//! directly (git shelling, IDE file I/O, agent-config file listing, provider probing) rather
//! than being thin `coducktor-core` delegates the way that crate's own module doc promises, so
//! porting the *whole* `Engine` trait honestly is a bigger lift than this one step's text
//! implies. The complete port now lives here: each family is implemented against the
//! matching `coducktor-server` handler, with the final trait adapter in `engine.rs`.
//!
//! **Status: complete.** `InProcessEngine` implements every method in `Engine`; scoped calls
//! intentionally resolve against this instance's one configured repository, and the adapter
//! converts the legacy raw JSON repository UI-state helpers to the typed trait contract.
//!
//! Every method here cites the `coducktor-server` handler it was ported from (that crate is
//! this port's oracle, the same role `packages/coducktor` played for the rest of Phase B) —
//! `coducktor-server` is deleted whole at C2, so duplicating its business logic here now
//! (rather than trying to share it across an axum-shaped and a non-axum-shaped caller) is the
//! right amount of engineering, not a shortcut.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coducktor_contract::{
    AgentAccountDetailsResponse, AgentAccountFile, AgentAccountStatusResponse, AgentConfigFile,
    AgentConfigFileContent, AgentConfigFormat, AgentConfigKind, AgentConfigListing,
    AgentConfigScope, AgentConfigTracked, AgentProfile, AgentProfileResponse,
    AgentProfileSelectionsResponse, AgentProfilesResponse, ApiRun, ArchiveFinishedResponse,
    BackendCheck, BackendCheckName, CancelAutoResumeResponse, CancelResponse, Capabilities,
    ChangedFile, ChangedFileStatus, ChangesPayload, ConfigResponse, ContinueInput,
    ContinueResponse, CreateAgentProfileInput, CreatePrResponse, CreateRunInput, CreateRunResponse,
    DeleteRunResponse, DeleteWorkflowResponse, EditQueuedMessageResponse, EmptyRepoResponse,
    FinishResponse, ForgeInfo, ForgeKind, GitCommitInput, GitCommitResponse, GitPushResponse,
    GithubChecksAvailable, GithubChecksData, GithubChecksUnavailable, GithubCommentsData,
    GithubData, GithubItemKind, GithubMergeInput, GithubMergeResponse, GithubPrChangesAvailable,
    GithubPrChangesData, GithubPrChangesUnavailable, GithubPrMergeStateResponse,
    GithubRefStatusAvailable, GithubRefStatusData, GithubRefStatusUnavailable, GroupResponse,
    GroupVariant, HealthProject, HealthResponse, IdeDirectoryResponse, IdeEntry, IdeEntryType,
    IdeFileResponse, ImageInput, LogEntry, MarkAllReadResponse, MessageInput, MessageResponse,
    ModelCatalogSource, ModelDiscoveryRunner, OpenAgentAccountFileInput,
    OpenAgentAccountFileResponse, OpenInCliResponse, OpenInInput, OpenProjectInResponse,
    OpenTargetsResponse, ParsedWorkflow, PatchRunInput, PickVariantRequest, PickVariantResponse,
    PlanResponse, PresentRepoResponse, ProjectListEntry, ProjectSource, ProjectStatus,
    ProjectsResponse, ProviderConnectionState, ProviderStatus, ProviderStatusResponse,
    QueuedMessagePatchInput, RUN_HISTORY_PAGE_ITEMS, ReclaimWorktreesResponse,
    RemoveAgentProfileResponse, RemoveProjectResponse, RemoveQueuedMessageResponse,
    RemoveTodoResponse, RemoveWorktreeResponse, RepoBranchRequest, RepoBranchResponse,
    RepoCommitPayload, RepoDiffStat, RepoInfo, RepoResponse, RunCommit, RunCommitsResponse,
    RunEvent, RunHistoryContext, RunHistoryEvent, RunHistoryPage, RunIndexEntry, Runner,
    RunnerModelCatalogResponse, RunnerModelOption, RunnerSelection, RunsIndexResponse,
    SaveWorkflowInput, SaveWorkflowResponse, SelectAgentProfileInput, SetAgentConfigInput,
    SetConfigInput, SetWorkspaceConfigInput, SetWorkspaceUiStateInput, Skill, StartTodoResponse,
    StatusEntry, TodoItem, UpdateAgentProfileInput, UpdateProjectInput, UpdateProjectResponse,
    UserMcpListing, WorkflowStepDef, WorkflowsResponse, WorkspaceConfigResponse, WorkspaceUiState,
    WorkspaceUsageResponse, WorktreeDirEntry, WorktreeEntry, WorktreeEntryType, WorktreeInfo,
    WorktreeRunStatus, WorktreesResponse,
};
use coducktor_core::config::load_config;
use coducktor_core::handoff::followups_enabled;
use coducktor_core::handoff::{append_handoff_heartbeat, handoff_progress_excerpt, read_handoff};
use coducktor_core::paths::{
    ProcessEnv, agent_accounts_path, agent_home_paths, expand_tilde, is_absolute_config_dir,
    real_home_dir,
};
use coducktor_core::skills::discover_skills;
use coducktor_core::workflows::load::{WORKFLOWS_DIR, load_workflows};
use coducktor_core::workflows::run::{
    EventInput, RunManager, StartRunInput as CoreStartRunInput, review_gate_enabled,
};
use coducktor_core::workflows::types::{parse_workflow_file_doc, quick_task_workflow, steps_issue};
use coducktor_core::workspace::agent_accounts::{
    AgentAccount, has_control_chars, is_valid_account_id, merge_write_agent_accounts,
    supports_profiles,
};
use coducktor_core::workspace::config::{
    PROVIDER_IDS, load_workspace_config, merge_write_workspace_config,
};
use coducktor_core::workspace::ui_state::{
    merge_write_workspace_ui_state, read_workspace_ui_state,
};
use coducktor_forge::{
    DraftPrInput, DraftPrOutcome, ForgeMergeInput, ForgeMergeResult, ForgePrDiffResult,
    ForgePrMergeStateResult, GithubDriver, resolve_forge,
};
use coducktor_runners::session_factory::DefaultSessionFactory;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;

use crate::Topic;
use crate::error::EngineError;
use crate::events::EngineEvent;

/// Version string this engine reports through `health()` — set once at construction, same as
/// `coducktor-server`'s `ServerConfig::version`.
pub struct InProcessEngine {
    repo_root: PathBuf,
    version: String,
    manager: Arc<Mutex<RunManager>>,
    live_events: broadcast::Sender<EngineEvent>,
    model_catalog: Arc<Mutex<Vec<CachedModelCatalog>>>,
}

/// Ported from `coducktor-server`'s private struct of the same name — a 5-minute TTL cache so a
/// slow/failing `codex`/`opencode` model probe doesn't re-run on every keystroke of a picker.
#[derive(Debug, Clone)]
struct CachedModelCatalog {
    runner: ModelDiscoveryRunner,
    models: Vec<RunnerModelOption>,
    expires_at: Instant,
    failure_reason: Option<String>,
}

const MODEL_CATALOG_TTL: Duration = Duration::from_secs(5 * 60);
const MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const CODEX_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MODEL_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_DISCOVERED_MODELS: usize = 500;

fn data_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".ai").join("coducktor")
}

fn lock_err() -> EngineError {
    EngineError::Unavailable {
        reason: "run manager unavailable".to_owned(),
    }
}

fn io_err(error: std::io::Error) -> EngineError {
    EngineError::Transport(error.to_string())
}

/// Duplicated from `coducktor-server`'s private helper of the same name — used by the
/// variant-group family (`pick_variant`) to record a `lifecycle` event without a full
/// `EventInput` builder call at each site.
fn lifecycle_event(message: String) -> EventInput {
    let mut event = EventInput::new("lifecycle");
    event
        .extra
        .insert("message".to_owned(), Value::String(message));
    event
}

impl InProcessEngine {
    /// Build a manager over `repo_root` wired with the real [`DefaultSessionFactory`] (B10) —
    /// the same production wiring `coducktor-tui`'s `serve`/`run` subcommands already use.
    /// Mirrors `coducktor-server::ServerState::with_manager_and_workspace_dir`'s event-fan-out
    /// wiring exactly, minus the WS/SSE transport: `subscribe_events`/`subscribe_runs`
    /// closures publish straight onto an in-process `broadcast` channel.
    pub fn new(repo_root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self::with_session_factory(repo_root, version, DefaultSessionFactory::new())
    }

    /// Same as [`Self::new`], but over an explicit [`SessionFactory`] — the seam a test (or a
    /// future non-`DefaultSessionFactory` embedder) uses instead of `DefaultSessionFactory`'s
    /// own live-process-environment snapshot.
    pub fn with_session_factory(
        repo_root: impl Into<PathBuf>,
        version: impl Into<String>,
        session_factory: impl coducktor_core::workflows::run::SessionFactory + 'static,
    ) -> Self {
        let repo_root = repo_root.into();
        let mut manager = RunManager::with_session_factory(data_dir(&repo_root), session_factory);
        let (live_events, _) = broadcast::channel(512);

        let event_sender = live_events.clone();
        manager.subscribe_events(move |notification| {
            let event = EngineEvent {
                topic: format!("run:{}", notification.run_id),
                data: json!({ "type": "run-event", "event": notification.event }),
            };
            let _ = event_sender.send(event);
        });
        let run_sender = live_events.clone();
        manager.subscribe_runs(move |run| {
            let data = json!({ "type": "run", "run": run });
            let event = EngineEvent {
                topic: format!("run:{}", run.id),
                data: data.clone(),
            };
            let _ = run_sender.send(event);
            let _ = run_sender.send(EngineEvent {
                topic: "workspace".to_owned(),
                data,
            });
        });

        Self {
            repo_root,
            version: version.into(),
            manager: Arc::new(Mutex::new(manager)),
            live_events,
            model_catalog: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    // ---- health (ported from `coducktor-server::health_payload`/`backend_check`) ----------

    pub async fn health(&self) -> Result<HealthResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let version = self.version.clone();
        tokio::task::spawn_blocking(move || health_payload(&repo_root, &version, false))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    /// Run the slower provider/version probes used by `coducktor doctor`. The interactive TUI
    /// deliberately uses the cheap health path so a missing or slow agent CLI cannot delay the
    /// first frame; settings and task execution perform their own provider-specific probes when
    /// the user asks for them.
    pub async fn diagnostic_health(&self) -> Result<HealthResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let version = self.version.clone();
        tokio::task::spawn_blocking(move || health_payload(&repo_root, &version, true))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    // ---- runs family (ported from the matching handlers in `coducktor-server::lib`) -------

    pub async fn list_runs(&self) -> Result<Vec<ApiRun>, EngineError> {
        let manager = self.manager.lock().map_err(|_| lock_err())?;
        Ok(manager.list_runs().into_iter().map(api_run).collect())
    }

    pub async fn get_run(&self, run_id: &str) -> Result<ApiRun, EngineError> {
        let manager = self.manager.lock().map_err(|_| lock_err())?;
        manager
            .get_run(run_id)
            .cloned()
            .map(api_run)
            .ok_or(EngineError::NotFound)
    }

    /// Mirrors `create_run`'s handler exactly, including its `variants` validation.
    pub async fn start_run(&self, input: CreateRunInput) -> Result<CreateRunResponse, EngineError> {
        let workflow = {
            let repo_root = self.repo_root.clone();
            let name = input.workflow.clone();
            let steps = input.steps.clone();
            if let Some(steps) = steps {
                if steps.is_empty() {
                    return Err(EngineError::Conflict {
                        reason: "steps must not be empty".to_owned(),
                    });
                }
                coducktor_contract::WorkflowDef {
                    name: "(planned)".to_owned(),
                    description: None,
                    steps,
                    source: coducktor_contract::WorkflowSource::BuiltIn,
                    path: None,
                }
            } else {
                let Some(name) = name else {
                    return Err(EngineError::Conflict {
                        reason: "workflow or steps is required".to_owned(),
                    });
                };
                load_workflows(&repo_root)
                    .0
                    .into_iter()
                    .find(|workflow| workflow.name == name)
                    .ok_or(EngineError::NotFound)?
            }
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
        let variants = input.variants.unwrap_or(1.0);
        if !variants.is_finite() || variants.fract() != 0.0 || !(1.0..=3.0).contains(&variants) {
            return Err(EngineError::Conflict {
                reason: "variants must be an integer from 1 to 3".to_owned(),
            });
        }
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if variants > 1.0 {
            let runs = manager
                .start_variants(&workflow, core_input, variants as usize)
                .map_err(io_err)?;
            return Ok(CreateRunResponse::Group { runs });
        }
        let run = manager.start_run(&workflow, core_input).map_err(io_err)?;
        Ok(CreateRunResponse::Single(Box::new(run)))
    }

    pub async fn archive_run(&self, run_id: &str, archived: bool) -> Result<ApiRun, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        manager
            .archive(run_id, archived)
            .map_err(io_err)?
            .map(api_run)
            .ok_or(EngineError::NotFound)
    }

    pub async fn delete_run(&self, run_id: &str) -> Result<DeleteRunResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        if manager.is_active(run_id) {
            return Err(EngineError::Conflict {
                reason: "cannot delete an active run".to_owned(),
            });
        }
        let deleted = manager.remove_run(run_id).map_err(io_err)?;
        Ok(DeleteRunResponse { deleted })
    }

    pub async fn read_run(&self, run_id: &str) -> Result<ApiRun, EngineError> {
        self.mutate_read(run_id, true).await
    }

    pub async fn unread_run(&self, run_id: &str) -> Result<ApiRun, EngineError> {
        self.mutate_read(run_id, false).await
    }

    async fn mutate_read(&self, run_id: &str, read: bool) -> Result<ApiRun, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let result = if read {
            manager.mark_read(run_id)
        } else {
            manager.mark_unread(run_id)
        };
        result
            .map_err(io_err)?
            .map(api_run)
            .ok_or(EngineError::NotFound)
    }

    pub async fn archive_finished(&self) -> Result<ArchiveFinishedResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let archived = manager.archive_finished().map_err(io_err)?;
        Ok(ArchiveFinishedResponse {
            archived: archived as f64,
        })
    }

    pub async fn mark_all_read(&self) -> Result<MarkAllReadResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let read = manager.mark_all_read().map_err(io_err)?;
        Ok(MarkAllReadResponse { read: read as f64 })
    }

    pub async fn patch_run(
        &self,
        run_id: &str,
        input: PatchRunInput,
    ) -> Result<ApiRun, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let current = manager
            .get_run(run_id)
            .cloned()
            .ok_or(EngineError::NotFound)?;
        if input.task.is_some() && current.status != coducktor_contract::RunStatus::Queued {
            return Err(EngineError::Conflict {
                reason: "run already started".to_owned(),
            });
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
        manager
            .update_run_value(run_id, Value::Object(value))
            .map_err(|error| EngineError::Conflict {
                reason: error.to_string(),
            })?
            .map(api_run)
            .ok_or(EngineError::NotFound)
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<CancelResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        let cancelled = manager.cancel(run_id).map_err(io_err)?;
        Ok(CancelResponse { cancelled })
    }

    pub async fn finish_run(&self, run_id: &str) -> Result<FinishResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        let finished = manager.finish(run_id).map_err(io_err)?;
        Ok(FinishResponse { finished })
    }

    /// Ported from `workspace_runs_index` — the cross-project global Tasks scan. Opens a
    /// fresh, throwaway `RunManager` for every OTHER registered project (this engine's own
    /// manager already holds the boot project's live state, so that one is reused instead of
    /// reopened).
    pub async fn runs_index(&self) -> Result<RunsIndexResponse, EngineError> {
        const PER_PROJECT_LIMIT: usize = 200;
        let config = load_workspace_config(
            &coducktor_core::paths::workspace_config_path(&ProcessEnv),
            &ProcessEnv,
        );
        let boot_root = self
            .repo_root
            .canonicalize()
            .unwrap_or_else(|_| self.repo_root.clone());
        let mut runs = Vec::new();
        let mut truncated = Vec::new();
        for project in config.projects {
            let root = PathBuf::from(&project.root);
            if !root.is_dir() {
                continue;
            }
            let mut recent = if root.canonicalize().ok().as_ref() == Some(&boot_root) {
                let manager = self.manager.lock().map_err(|_| lock_err())?;
                manager.list_runs()
            } else {
                RunManager::open(root.join(".ai").join("coducktor")).list_runs()
            };
            recent.sort_by(|left, right| right.created_at.cmp(&left.created_at));
            if recent.len() > PER_PROJECT_LIMIT {
                truncated.push(project.id.clone());
            }
            runs.extend(
                recent
                    .into_iter()
                    .take(PER_PROJECT_LIMIT)
                    .map(|run| run_index_entry(&project.id, run)),
            );
        }
        runs.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(RunsIndexResponse {
            runs,
            reference_statuses: BTreeMap::new(),
            per_project_limit: PER_PROJECT_LIMIT as u64,
            truncated,
        })
    }

    // ---- workflows + skills (ported from `list_workflows`/`list_skills`) -------------------

    pub async fn workflows(&self) -> Result<WorkflowsResponse, EngineError> {
        let (workflows, issues) = load_workflows(&self.repo_root);
        Ok(WorkflowsResponse { workflows, issues })
    }

    pub async fn skills(&self) -> Result<Vec<Skill>, EngineError> {
        Ok(discover_skills(&self.repo_root, &ProcessEnv))
    }

    // ---- workflow builder writes (ported from `save_workflow_at`/`delete_workflow_at`/
    // `parse_workflow_input`) ----------------------------------------------------------------

    /// Ported from `save_workflow_at`. `workflow_slug`/`workflow_step_issue`/`workflow_input`/
    /// `workflow_yaml` below are copied from `coducktor-server`'s own (private, non-`pub`)
    /// helpers of the same name rather than shared — that crate is deleted whole at C2, so
    /// duplicating this validation/YAML-generation logic now is the same deliberate call this
    /// module's doc already makes for the rest of C1, not an oversight.
    pub async fn save_workflow(
        &self,
        input: &SaveWorkflowInput,
    ) -> Result<SaveWorkflowResponse, EngineError> {
        let (name, description, steps, compact) =
            workflow_input(input).map_err(|reason| EngineError::Conflict { reason })?;
        let directory = self.repo_root.join(WORKFLOWS_DIR);
        let path = directory.join(format!("{}.yaml", workflow_slug(&name)));
        let yaml = workflow_yaml(&name, description.as_deref(), &steps, compact)
            .map_err(|reason| EngineError::Conflict { reason })?;
        std::fs::create_dir_all(&directory).map_err(io_err)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true);
        if input.overwrite.unwrap_or(false) {
            options.truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(EngineError::Conflict {
                    reason: format!("workflow file already exists: {}", path.display()),
                });
            }
            Err(error) => return Err(io_err(error)),
        };
        use std::io::Write as _;
        file.write_all(yaml.as_bytes()).map_err(io_err)?;
        Ok(SaveWorkflowResponse {
            path: path.to_string_lossy().into_owned(),
            name,
        })
    }

    /// Ported from `delete_workflow_at`.
    pub async fn delete_workflow(&self, name: &str) -> Result<DeleteWorkflowResponse, EngineError> {
        let (workflows, _) = load_workflows(&self.repo_root);
        let workflow = workflows
            .into_iter()
            .find(|workflow| workflow.name == name)
            .ok_or(EngineError::NotFound)?;
        let Some(path) = workflow.path else {
            return Err(EngineError::Conflict {
                reason: "built-in workflows cannot be deleted".to_owned(),
            });
        };
        let directory = self.repo_root.join(WORKFLOWS_DIR);
        let target = PathBuf::from(&path);
        if !target.starts_with(&directory) {
            return Err(EngineError::Conflict {
                reason: "refusing to delete a file outside the workflows dir".to_owned(),
            });
        }
        std::fs::remove_file(&target).map_err(io_err)?;
        Ok(DeleteWorkflowResponse {
            ok: true,
            path: target.to_string_lossy().into_owned(),
        })
    }

    /// Ported from `parse_workflow_input`.
    pub async fn parse_workflow(&self, yaml: &str) -> Result<ParsedWorkflow, EngineError> {
        if yaml.trim().is_empty() || yaml.chars().count() > 100_000 {
            return Err(EngineError::Conflict {
                reason: "yaml must be between 1 and 100000 characters".to_owned(),
            });
        }
        let value: Value =
            serde_yaml_ng::from_str(yaml).map_err(|error| EngineError::Conflict {
                reason: format!("not valid YAML: {error}"),
            })?;
        let (name, description, steps) =
            parse_workflow_file_doc(&value).map_err(|reason| EngineError::Conflict { reason })?;
        Ok(ParsedWorkflow {
            name,
            description,
            steps,
        })
    }

    // ---- ui-state (ported from `get_ui_state`/`update_ui_state`) ---------------------------

    pub async fn ui_state(&self) -> Result<Value, EngineError> {
        Ok(Value::Object(read_repo_ui_state(&self.repo_root)))
    }

    pub async fn put_ui_state(&self, input: Value) -> Result<Value, EngineError> {
        let path = repo_ui_state_path(&self.repo_root);
        let mut current = read_repo_ui_state(&self.repo_root);
        let Value::Object(patch) = input else {
            return Err(EngineError::Conflict {
                reason: "ui-state patch must be a JSON object".to_owned(),
            });
        };
        for (key, value) in patch {
            current.insert(key, value);
        }
        let serialized = serde_json::to_vec_pretty(&Value::Object(current.clone()))
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        std::fs::write(&path, serialized).map_err(io_err)?;
        Ok(Value::Object(current))
    }

    // ---- follow-up inbox (ported from `list_todos`/`delete_todo`/`start_todo`) -------------

    pub async fn todos(&self) -> Result<Vec<TodoItem>, EngineError> {
        if !followups_enabled(&ProcessEnv) {
            return Ok(Vec::new());
        }
        Ok(coducktor_core::todos::read_todos(&data_dir(
            &self.repo_root,
        )))
    }

    pub async fn delete_todo(&self, id: &str) -> Result<RemoveTodoResponse, EngineError> {
        if !followups_enabled(&ProcessEnv) {
            return Err(EngineError::Conflict {
                reason: "the follow-up inbox is disabled — set DUCK_FOLLOWUPS=1 to enable it"
                    .to_owned(),
            });
        }
        let removed =
            coducktor_core::todos::remove_todo(&data_dir(&self.repo_root), id).map_err(io_err)?;
        if !removed {
            return Err(EngineError::NotFound);
        }
        Ok(RemoveTodoResponse { removed: true })
    }

    /// Runs the todo's own suggested skill (or a bare quick-task) with its saved prompt.
    /// The `Engine` trait intentionally exposes no runner/model override for this action, so
    /// the saved todo workflow supplies those choices just as it does for the HTTP no-body path.
    pub async fn start_todo(&self, id: &str) -> Result<StartTodoResponse, EngineError> {
        if !followups_enabled(&ProcessEnv) {
            return Err(EngineError::Conflict {
                reason: "the follow-up inbox is disabled — set DUCK_FOLLOWUPS=1 to enable it"
                    .to_owned(),
            });
        }
        let data_dir = data_dir(&self.repo_root);
        let todos = coducktor_core::todos::read_todos(&data_dir);
        let todo = todos
            .into_iter()
            .find(|todo| todo.id == id)
            .ok_or(EngineError::NotFound)?;
        if todo.started_task_id.is_some() {
            return Err(EngineError::Conflict {
                reason: "already started".to_owned(),
            });
        }
        let task = coducktor_core::todos::todo_task_text(
            &todo.summary,
            todo.suggested_prompt.as_deref(),
            todo.suggested_args.as_deref(),
        );
        let workflow = todo_workflow(&self.repo_root, &todo);
        let core_input = CoreStartRunInput {
            task,
            ..CoreStartRunInput::default()
        };
        let run = {
            let mut manager = self.manager.lock().map_err(|_| lock_err())?;
            manager.start_run(&workflow, core_input).map_err(io_err)?
        };
        match coducktor_core::todos::mark_started(&data_dir, id, &run.id) {
            Ok(true) => Ok(StartTodoResponse { run }),
            Ok(false) => Err(EngineError::Conflict {
                reason: "already started".to_owned(),
            }),
            Err(error) => Err(io_err(error)),
        }
    }

    // ---- workspace: projects (ported from `list_projects`; read-only for now) --------------

    pub async fn projects(&self) -> Result<ProjectsResponse, EngineError> {
        let config = load_workspace_config(
            &coducktor_core::paths::workspace_config_path(&ProcessEnv),
            &ProcessEnv,
        );
        let boot_project = boot_project_id(&config, &self.repo_root);
        let projects = config.projects.iter().map(project_entry).collect();
        Ok(ProjectsResponse {
            projects,
            boot_project,
            projects_dir: config.projects_dir,
        })
    }

    /// Ported from `get_workspace_usage` — the quota-telemetry stack (`core/quota/*`) was
    /// scope-cut entirely at B10 (unrelated to session execution, a meaningfully separate
    /// porting effort), so the server's own route already answers with an empty provider list
    /// rather than real telemetry. This mirrors that exactly, not a new gap introduced here.
    pub async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError> {
        Ok(WorkspaceUsageResponse { providers: vec![] })
    }

    // ---- provider status + agent-profile accounts (ported from the matching handlers in
    // `coducktor-server::lib`: `get_provider_status`, `list_agent_profiles`,
    // `create_agent_profile`, `update_agent_profile`, `remove_agent_profile`,
    // `select_agent_profile`, `get_agent_profile_status`, `get_agent_profile_details`,
    // `open_agent_profile_file`) -------------------------------------------------------------

    pub async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError> {
        tokio::task::spawn_blocking(provider_status_response)
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn agent_profiles(&self) -> Result<AgentProfilesResponse, EngineError> {
        Ok(agent_profiles_response())
    }

    /// Ported from `create_agent_profile`.
    pub async fn create_agent_profile(
        &self,
        input: &CreateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        if !supports_profiles(input.provider) {
            return Err(EngineError::Conflict {
                reason: format!(
                    "{} cannot carry more than one account",
                    serde_json::to_string(&input.provider)
                        .unwrap_or_default()
                        .trim_matches('"')
                ),
            });
        }
        let config_dir = input.config_dir.trim().to_owned();
        if let Some(error) = profile_path_error(&config_dir) {
            return Err(EngineError::Conflict { reason: error });
        }
        let path = expand_tilde(&config_dir, &ProcessEnv);
        let store_path = agent_accounts_path(&ProcessEnv);
        let current = coducktor_core::workspace::agent_accounts::load_agent_accounts(&store_path);
        if let Some(error) = profile_conflict(&current, input.provider, &path, None) {
            return Err(EngineError::Conflict { reason: error });
        }
        let source = input
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(str::trim)
            .or_else(|| path.file_name().and_then(|name| name.to_str()))
            .unwrap_or("account");
        let taken = current
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let id = allocate_account_id(source, &taken);
        if !is_valid_account_id(&id) {
            return Err(EngineError::Conflict {
                reason: "invalid account id".to_owned(),
            });
        }
        let label = input
            .label
            .clone()
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| id.clone());
        let added = AgentAccount {
            id,
            provider: input.provider,
            config_dir,
            label,
            added_at: coducktor_core::time::now_iso8601(),
            extra: Default::default(),
        };
        let added_id = added.id.clone();
        let saved = merge_write_agent_accounts(&store_path, |store| store.accounts.push(added))
            .map_err(io_err)?;
        let account = saved
            .accounts
            .iter()
            .find(|account| account.id == added_id)
            .ok_or_else(|| EngineError::Transport("account could not be saved".to_owned()))?;
        Ok(AgentProfileResponse {
            profile: agent_profile_wire(&resolved_agent_profile(account)),
        })
    }

    /// Ported from `update_agent_profile`.
    pub async fn update_agent_profile(
        &self,
        id: &str,
        input: &UpdateAgentProfileInput,
    ) -> Result<AgentProfileResponse, EngineError> {
        if input.label.is_none() && input.config_dir.is_none() {
            return Err(EngineError::Conflict {
                reason: "send label or configDir".to_owned(),
            });
        }
        let store_path = agent_accounts_path(&ProcessEnv);
        let current = coducktor_core::workspace::agent_accounts::load_agent_accounts(&store_path);
        let Some(existing) = current.accounts.iter().find(|account| account.id == id) else {
            return Err(EngineError::NotFound);
        };
        let new_path = if let Some(config_dir) = &input.config_dir {
            let config_dir = config_dir.trim();
            if let Some(error) = profile_path_error(config_dir) {
                return Err(EngineError::Conflict { reason: error });
            }
            Some(expand_tilde(config_dir, &ProcessEnv))
        } else {
            None
        };
        if let Some(path) = &new_path
            && let Some(error) = profile_conflict(&current, existing.provider, path, Some(id))
        {
            return Err(EngineError::Conflict { reason: error });
        }
        let id_owned = id.to_owned();
        let input = input.clone();
        let mut updated = None;
        let saved = merge_write_agent_accounts(&store_path, |store| {
            let Some(account) = store
                .accounts
                .iter_mut()
                .find(|account| account.id == id_owned)
            else {
                return;
            };
            if let Some(label) = &input.label {
                let label = label.trim();
                account.label = if label.is_empty() {
                    account.id.clone()
                } else {
                    label.to_owned()
                };
            }
            if let Some(config_dir) = &input.config_dir {
                account.config_dir = config_dir.trim().to_owned();
            }
            updated = Some(account.clone());
        })
        .map_err(io_err)?;
        let account =
            updated.or_else(|| saved.accounts.into_iter().find(|account| account.id == id));
        let Some(account) = account else {
            return Err(EngineError::NotFound);
        };
        Ok(AgentProfileResponse {
            profile: agent_profile_wire(&resolved_agent_profile(&account)),
        })
    }

    /// Ported from `remove_agent_profile`.
    pub async fn remove_agent_profile(
        &self,
        id: &str,
    ) -> Result<RemoveAgentProfileResponse, EngineError> {
        let store_path = agent_accounts_path(&ProcessEnv);
        let current = coducktor_core::workspace::agent_accounts::load_agent_accounts(&store_path);
        if !current.accounts.iter().any(|account| account.id == id) {
            return Err(EngineError::NotFound);
        }
        let id_owned = id.to_owned();
        merge_write_agent_accounts(&store_path, |store| {
            store.accounts.retain(|account| account.id != id_owned);
            for (_, selection) in &mut store.selections {
                if selection.claude.as_deref() == Some(&id_owned) {
                    selection.claude = None;
                }
                if selection.codex.as_deref() == Some(&id_owned) {
                    selection.codex = None;
                }
                if selection.opencode.as_deref() == Some(&id_owned) {
                    selection.opencode = None;
                }
                if selection.pi.as_deref() == Some(&id_owned) {
                    selection.pi = None;
                }
            }
            store
                .selections
                .retain(|(_, selection)| !selection_empty(selection));
        })
        .map_err(io_err)?;
        Ok(RemoveAgentProfileResponse {
            removed: true,
            id: id.to_owned(),
        })
    }

    /// Ported from `select_agent_profile`.
    pub async fn select_agent_profile(
        &self,
        input: &SelectAgentProfileInput,
    ) -> Result<AgentProfileSelectionsResponse, EngineError> {
        let root = project_root_for_agent_selection(&self.repo_root, input.project_id.as_deref());
        if input.project_id.is_some() && root.is_none() {
            return Err(EngineError::NotFound);
        }
        let store_path = agent_accounts_path(&ProcessEnv);
        let current = coducktor_core::workspace::agent_accounts::load_agent_accounts(&store_path);
        if let Some(profile_id) = input.profile_id.as_deref()
            && profile_id != coducktor_contract::DEFAULT_AGENT_ACCOUNT_ID
            && !current
                .accounts
                .iter()
                .any(|account| account.id == profile_id && account.provider == input.provider)
        {
            return Err(EngineError::Conflict {
                reason: format!("unknown {:?} account: {profile_id}", input.provider)
                    .to_lowercase(),
            });
        }
        let profile_id = input
            .profile_id
            .clone()
            .filter(|profile_id| profile_id != coducktor_contract::DEFAULT_AGENT_ACCOUNT_ID);
        let root_key = root.map(|path| path.to_string_lossy().into_owned());
        let provider = input.provider;
        let saved = merge_write_agent_accounts(&store_path, |store| {
            if let Some(root) = &root_key {
                if let Some((_, selection)) =
                    store.selections.iter_mut().find(|(key, _)| key == root)
                {
                    set_profile_selection(selection, provider, profile_id.clone());
                    if selection_empty(selection) {
                        store.selections.retain(|(key, _)| key != root);
                    }
                } else if let Some(profile_id) = profile_id.clone() {
                    let mut selection =
                        coducktor_core::workspace::agent_accounts::AgentAccountSelection::default();
                    set_profile_selection(&mut selection, provider, Some(profile_id));
                    store.selections.push((root.clone(), selection));
                }
            } else {
                set_profile_selection(&mut store.defaults, provider, profile_id.clone());
            }
        })
        .map_err(io_err)?;
        let selections = saved
            .selections
            .iter()
            .map(|(root, selection)| (root.clone(), selection_wire(selection)))
            .collect();
        Ok(AgentProfileSelectionsResponse {
            selections,
            defaults: selection_wire(&saved.defaults),
        })
    }

    /// Ported from `get_agent_profile_status`. `refresh` is accepted for signature parity with
    /// network engine but has no effect — the oracle's own handler ignores it too (there is no
    /// caching layer for provider status on either side; every call already probes fresh).
    pub async fn agent_account_status(
        &self,
        id: &str,
        _refresh: bool,
    ) -> Result<AgentAccountStatusResponse, EngineError> {
        let accounts_path = agent_accounts_path(&ProcessEnv);
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let profile = account_by_route_id(&accounts_path, &id).ok_or(EngineError::NotFound)?;
            Ok(AgentAccountStatusResponse {
                status: provider_status_for_profile(&profile),
            })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    /// Ported from `get_agent_profile_details`.
    pub async fn agent_account_details(
        &self,
        id: &str,
    ) -> Result<AgentAccountDetailsResponse, EngineError> {
        let accounts_path = agent_accounts_path(&ProcessEnv);
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let profile = account_by_route_id(&accounts_path, &id).ok_or(EngineError::NotFound)?;
            Ok(agent_profile_details(&profile))
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    /// Ported from `open_agent_profile_file`; explicit app targets reuse this module's
    /// `open_targets` registry and launcher, while `target: None` uses the OS default opener.
    pub async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError> {
        if let Some(target) = input.target.as_deref() {
            if target.starts_with("cli:") {
                return Err(EngineError::Conflict {
                    reason: "agent CLIs open a task worktree, not a config folder".to_owned(),
                });
            }
            if target == "terminal" && input.file != "folder" {
                return Err(EngineError::Conflict {
                    reason: "a terminal opens a folder, not a file".to_owned(),
                });
            }
            if !open_targets_list()
                .iter()
                .any(|candidate| candidate.id == target)
            {
                return Err(EngineError::Conflict {
                    reason: format!("no such app on this machine: {target}"),
                });
            }
        }
        let accounts_path = agent_accounts_path(&ProcessEnv);
        let id = id.to_owned();
        let file = input.file.clone();
        let target = input.target.clone();
        tokio::task::spawn_blocking(move || {
            let profile = account_by_route_id(&accounts_path, &id).ok_or(EngineError::NotFound)?;
            let is_folder = file == "folder";
            let path = if is_folder {
                profile.path.clone()
            } else if let Some(found) = profile_files(&profile).into_iter().find(|f| f.id == file) {
                PathBuf::from(found.path)
            } else {
                return Err(EngineError::NotFound);
            };
            if !is_folder && std::fs::metadata(&path).is_err() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file");
                return Err(EngineError::Conflict {
                    reason: format!("this account has no {name} yet"),
                });
            }
            let opened = target
                .as_deref()
                .map(|target| open_target(&path, target))
                .unwrap_or_else(|| account_open_default(&path));
            if !opened {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file");
                return Err(EngineError::Conflict {
                    reason: format!("could not open {name}"),
                });
            }
            Ok(OpenAgentAccountFileResponse {
                opened: true,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    // ---- live events (Topic::Health/Todos/Run/Named -> the in-process broadcast channel) ---

    /// Mirrors the former network engine's topic-string convention, but the transport is
    /// a plain in-process `tokio::sync::broadcast` receiver instead of a WS frame — no
    /// reconnect/resubscribe machinery needed, there is no connection to lose.
    pub fn subscribe(&self, topic: Topic) -> futures_core::stream::BoxStream<'static, EngineEvent> {
        let topic_str = match topic {
            Topic::Health => "health".to_owned(),
            Topic::Todos => "todos".to_owned(),
            Topic::Run { id } => format!("run:{id}"),
            Topic::Named(topic) => topic,
        };
        let receiver = self.live_events.subscribe();
        Box::pin(
            BroadcastStream::new(receiver)
                .filter_map(|item| item.ok())
                .filter(move |event: &EngineEvent| event.topic == topic_str),
        )
    }

    // ---- IDE: project file browser + editor (spec §8.8, A10) --------------------------------
    // Ported from `coducktor-server`'s `ide_list_directory`/`ide_read_file`/`ide_write_file` +
    // their shared `resolve_ide_path`/`normalize_ide_path`/`ide_display_path` helpers. `Scope`
    // is dropped the same way every other method here drops it — `coducktor-server`'s own
    // "scoped" IDE routes already ignore their `:project` path segment and always resolve
    // against `state.config.repo_root`, since this crate (like that one) serves exactly one
    // repo root per instance.

    pub async fn ide_tree(&self, path: Option<&str>) -> Result<IdeDirectoryResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let path = path.unwrap_or_default().to_owned();
        tokio::task::spawn_blocking(move || ide_list_directory(&repo_root, &path))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn ide_file(&self, path: &str) -> Result<IdeFileResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || ide_read_file(&repo_root, &path))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn ide_save(
        &self,
        path: &str,
        content: &str,
    ) -> Result<IdeFileResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let path = path.to_owned();
        let content = content.to_owned();
        tokio::task::spawn_blocking(move || ide_write_file(&repo_root, &path, &content))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    // ---- per-repo config (spec §8.14, settings) ----------------------------------------------
    // Ported from `coducktor-server`'s `config_response`/`parse_set_config_input`/
    // `config_models_locked`/`read_repo_config` handlers. Unlike the HTTP handler, `put_config`
    // here receives an already-typed `&SetConfigInput` directly (no JSON-parse boundary), so
    // the outer/inner `Option<Option<T>>` "field absent vs. field present-but-null" distinction
    // the handler has to reconstruct from a raw `Map<String, Value>` alongside the typed struct
    // is already exactly right on the struct itself — no parallel raw-object bookkeeping needed.

    pub async fn config(&self) -> Result<ConfigResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || config_response(&repo_root))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn put_config(&self, input: &SetConfigInput) -> Result<ConfigResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let input = input.clone();
        tokio::task::spawn_blocking(move || update_repo_config(&repo_root, &input))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    // ---- diff engine: task git, repo git, compare (spec §8.5-§8.7, A9) -----------------------
    // Ported from `coducktor-server`'s `run_diff`/`run_changes`/`run_commit`/`run_files`/
    // `get_repo`/`get_repo_changes`/`get_repo_commit`/`create_repo_branch` handlers, plus their
    // shared `repo_info_at`/`repo_status`/`repo_log`/`repo_branches`/`collect_git_changes`/
    // `repo_commit_payload`/`read_worktree_path` helpers (all duplicated below — none were
    // `pub`). `group`/`pick_variant` below are the separate, more involved cluster (they mutate
    // run state — cancel/archive losing variants, remove their worktrees, touch the review gate).

    fn run_record(&self, run_id: &str) -> Result<coducktor_contract::RunRecord, EngineError> {
        let manager = self.manager.lock().map_err(|_| lock_err())?;
        manager
            .get_run(run_id)
            .cloned()
            .ok_or(EngineError::NotFound)
    }

    fn run_working_directory(&self, run: &coducktor_contract::RunRecord) -> Option<PathBuf> {
        if run.worktree == Some(false) {
            Some(self.repo_root.clone())
        } else {
            run_worktree_of(run)
        }
    }

    pub async fn run_diff_text(&self, run_id: &str) -> Result<String, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(worktree) = run_worktree_of(&run) else {
            return Ok(NO_WORKTREE.to_owned());
        };
        let base = run.base_branch.clone().unwrap_or_else(|| "HEAD".to_owned());
        tokio::task::spawn_blocking(move || {
            coducktor_core::git::worktree::worktree_diff(&worktree, &base, 400_000)
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn run_changes(&self, run_id: &str) -> Result<ChangesPayload, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(root) = self.run_working_directory(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        let base = run.base_branch.clone().unwrap_or_else(|| "HEAD".to_owned());
        tokio::task::spawn_blocking(move || {
            run_changes_payload(&root, &base).map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn run_commit(
        &self,
        run_id: &str,
        sha: &str,
    ) -> Result<RepoCommitPayload, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(root) = self.run_working_directory(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        let sha = sha.to_owned();
        tokio::task::spawn_blocking(move || {
            repo_commit_payload(&root, &sha).map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn run_files(
        &self,
        run_id: &str,
        path: Option<&str>,
    ) -> Result<WorktreeEntry, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(root) = self.run_working_directory(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        let relative = path.unwrap_or_default().to_owned();
        tokio::task::spawn_blocking(move || {
            read_worktree_path(&root, &relative).map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    /// Raw bytes for an image the worktree file browser can preview (matches the oracle's own
    /// `run_files?raw=1` restriction: "raw serving is limited to images").
    pub async fn run_file_raw(&self, run_id: &str, path: &str) -> Result<Vec<u8>, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(root) = self.run_working_directory(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        let relative = path.to_owned();
        tokio::task::spawn_blocking(move || read_worktree_raw(&root, &relative))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn repo(&self) -> Result<RepoResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || repo_response(&repo_root))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn repo_changes(&self) -> Result<ChangesPayload, EngineError> {
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            let Some(info) = repo_info_at(&repo_root) else {
                return Err(EngineError::Conflict {
                    reason: "not a git repository".to_owned(),
                });
            };
            collect_git_changes(Path::new(&info.root), &["HEAD".to_owned()])
                .map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn repo_commit(&self, sha: &str) -> Result<RepoCommitPayload, EngineError> {
        let repo_root = self.repo_root.clone();
        let sha = sha.to_owned();
        tokio::task::spawn_blocking(move || {
            let Some(info) = repo_info_at(&repo_root) else {
                return Err(EngineError::Conflict {
                    reason: "not a git repository".to_owned(),
                });
            };
            repo_commit_payload(Path::new(&info.root), &sha)
                .map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn repo_branch(
        &self,
        input: &RepoBranchRequest,
    ) -> Result<RepoBranchResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let input = input.clone();
        tokio::task::spawn_blocking(move || create_repo_branch(&repo_root, &input))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    // ---- agent-config (spec §8.14 "Agent config" section) -------------------------------------
    // Ported from `coducktor-server`'s `list_agent_config`/`get_agent_config`/
    // `update_agent_config` handlers, duplicating their private `AGENT_CONFIG_DEFINITIONS`
    // catalog and `resolve_agent_config_path`/`config_hash`/`agent_config_content`/
    // `jsonc_without_comments`/`validate_agent_config`/`claude_state_path`/`user_mcp_listing`/
    // `agent_config_listing`/`write_agent_config` helpers byte-for-byte (none were `pub`).

    pub async fn agent_config(&self) -> Result<AgentConfigListing, EngineError> {
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || agent_config_listing(&repo_root))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn agent_config_file(&self, id: &str) -> Result<AgentConfigFileContent, EngineError> {
        let repo_root = self.repo_root.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let definition = agent_config_definition(&id).ok_or(EngineError::NotFound)?;
            agent_config_content(definition, &repo_root).map_err(EngineError::Transport)
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn put_agent_config_file(
        &self,
        id: &str,
        input: &SetAgentConfigInput,
    ) -> Result<AgentConfigFileContent, EngineError> {
        if input.content.chars().count() > 2_000_000 {
            return Err(EngineError::Conflict {
                reason: "content must be at most 2000000 characters".to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        let id = id.to_owned();
        let input = input.clone();
        tokio::task::spawn_blocking(move || {
            let definition = agent_config_definition(&id).ok_or(EngineError::NotFound)?;
            write_agent_config(definition, &repo_root, input)
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    // ---- worktree management (spec §8.7, A9) --------------------------------------------------
    // Ported from `coducktor-server`'s `list_worktrees`/`reclaim_worktrees`/`remove_run_worktree`
    // handlers. Reuses `coducktor_core::runs::retention::{is_reclaimable, reclaim_worktrees}`
    // and `coducktor_core::config::resolve_worktree_retention` directly (already ported at
    // B2/B3) — only the response-shaping glue (`worktree_run_status`/`worktree_keep`) is
    // duplicated, since it was never `pub` in the oracle.

    fn worktree_keep(&self) -> u64 {
        let workspace = workspace_config_for(&self.repo_root);
        coducktor_core::config::resolve_worktree_retention(
            &self.repo_root,
            Some(workspace.resources.worktree_retention_default),
        )
    }

    pub async fn worktrees(&self) -> Result<WorktreesResponse, EngineError> {
        let keep = self.worktree_keep();
        let manager = self.manager.lock().map_err(|_| lock_err())?;
        let mut worktrees = Vec::new();
        let mut any_size_unavailable = false;
        let mut total = 0_u64;
        for run in manager.list_runs() {
            let Some(path) = run.worktree_path.as_deref() else {
                continue;
            };
            if !Path::new(path).exists() {
                continue;
            }
            let size = worktree_size_bytes(Path::new(path));
            if let Some(bytes) = size {
                total = total.saturating_add(bytes);
            } else {
                any_size_unavailable = true;
            }
            let reclaimable = coducktor_core::runs::retention::is_reclaimable(&run);
            let title = if run.title.is_empty() {
                run.id.clone()
            } else {
                run.title.clone()
            };
            worktrees.push(WorktreeInfo {
                run_id: run.id,
                title,
                status: worktree_run_status(run.status),
                branch: run.branch,
                size_bytes: size.map(|bytes| bytes as f64),
                finished_at: run.finished_at,
                reclaimable,
            });
        }
        Ok(WorktreesResponse {
            worktrees,
            total_bytes: (!any_size_unavailable).then_some(total),
            keep,
        })
    }

    pub async fn reclaim_worktrees(&self) -> Result<ReclaimWorktreesResponse, EngineError> {
        let keep = self.worktree_keep();
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let runs = manager.list_runs();
        let reclaimed = coducktor_core::runs::retention::reclaim_worktrees(
            &self.repo_root,
            &runs,
            keep,
            coducktor_core::time::now_iso8601,
        );
        let mut ids = Vec::new();
        for (id, timestamp) in reclaimed {
            if manager
                .edit_run(&id, |run| run.worktree_reclaimed_at = Some(timestamp))
                .is_ok()
            {
                ids.push(id);
            }
        }
        Ok(ReclaimWorktreesResponse { reclaimed: ids })
    }

    pub async fn remove_run_worktree(
        &self,
        run_id: &str,
    ) -> Result<RemoveWorktreeResponse, EngineError> {
        let run = self.run_record(run_id)?;
        {
            let manager = self.manager.lock().map_err(|_| lock_err())?;
            if manager.is_active(run_id) {
                return Err(EngineError::Conflict {
                    reason: "run is active — cancel it first".to_owned(),
                });
            }
        }
        if let Some(worktree) = run_worktree_of(&run) {
            let repo_root = self.repo_root.clone();
            let branch = run.branch.clone();
            tokio::task::spawn_blocking(move || {
                coducktor_core::git::worktree::remove_worktree(
                    &repo_root,
                    &worktree,
                    branch.as_deref(),
                )
            })
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        }
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        manager
            .update_run_value(
                run_id,
                serde_json::json!({ "worktreePath": null, "branch": null }),
            )
            .map_err(io_err)?;
        Ok(RemoveWorktreeResponse { removed: true })
    }

    // ---- variant groups (spec §8.6 compare) -------------------------------------------------
    // Ported from `coducktor-server`'s `group_variants`/`group_response`/`get_group` and
    // `parse_pick_variant`/`pick_group_at` handlers. `lifecycle_event` (below) duplicates the
    // server's own tiny helper of the same name — not `pub` in the oracle.

    fn group_variants(
        &self,
        group_id: &str,
    ) -> Result<Vec<coducktor_contract::RunRecord>, EngineError> {
        let manager = self.manager.lock().map_err(|_| lock_err())?;
        let mut runs: Vec<_> = manager
            .list_runs()
            .into_iter()
            .filter(|run| run.group_id.as_deref() == Some(group_id))
            .collect();
        runs.sort_by(|left, right| left.variant.cmp(&right.variant));
        Ok(runs)
    }

    pub async fn group(&self, group_id: &str) -> Result<GroupResponse, EngineError> {
        let group_id = group_id.to_owned();
        let repo_root = self.repo_root.clone();
        let runs = self.group_variants(&group_id)?;
        if runs.is_empty() {
            return Err(EngineError::NotFound);
        }
        let data_dir = data_dir(&repo_root);
        let runs = runs
            .into_iter()
            .map(|run| {
                let diff_stat = run
                    .worktree_path
                    .as_deref()
                    .filter(|path| Path::new(path).exists())
                    .map(|path| {
                        coducktor_core::git::worktree::worktree_diff_stat(
                            Path::new(path),
                            run.base_branch.as_deref().unwrap_or("HEAD"),
                        )
                    })
                    .unwrap_or_default();
                GroupVariant {
                    id: run.id.clone(),
                    variant: run.variant.unwrap_or_else(|| "?".to_owned()),
                    title: run.title,
                    status: run.status,
                    archived: run.archived,
                    tokens_used: run.tokens_used,
                    input_tokens: run.input_tokens,
                    output_tokens: run.output_tokens,
                    cost_usd: run.cost_usd,
                    diff_stat,
                    handoff_excerpt: handoff_progress_excerpt(&read_handoff(&data_dir, &run.id), 3),
                }
            })
            .collect();
        Ok(GroupResponse { group_id, runs })
    }

    pub async fn pick_variant(
        &self,
        group_id: &str,
        input: &PickVariantRequest,
    ) -> Result<PickVariantResponse, EngineError> {
        if input.run_id.is_empty() {
            return Err(EngineError::Conflict {
                reason: "runId is required".to_owned(),
            });
        }
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let runs: Vec<_> = manager
            .list_runs()
            .into_iter()
            .filter(|run| run.group_id.as_deref() == Some(group_id))
            .collect();
        if runs.is_empty() {
            return Err(EngineError::NotFound);
        }
        let Some(winner) = runs.iter().find(|run| run.id == input.run_id).cloned() else {
            return Err(EngineError::Conflict {
                reason: "runId is not part of this group".to_owned(),
            });
        };
        if manager.is_active(&winner.id) {
            return Err(EngineError::Conflict {
                reason: "this variant is still active — wait for it to finish first".to_owned(),
            });
        }

        let losers: Vec<_> = runs.into_iter().filter(|run| run.id != winner.id).collect();
        let repo_root = self.repo_root.clone();
        let data_dir = data_dir(&repo_root);
        let config = load_config(&repo_root, &workspace_config_for(&repo_root).agent_defaults);
        let review_gate = review_gate_enabled(
            config.review_gate,
            std::env::var("DUCK_REVIEW_GATE").ok().as_deref(),
        );
        if winner.status != coducktor_contract::RunStatus::Review
            && winner.autonomous != Some(true)
            && review_gate
            && let Some(worktree_path) = winner.worktree_path.as_deref()
            && Path::new(worktree_path).exists()
        {
            let diff = coducktor_core::git::worktree::worktree_diff(
                Path::new(worktree_path),
                winner.base_branch.as_deref().unwrap_or("HEAD"),
                1_000_000,
            );
            if !diff.trim().is_empty() && !diff.starts_with("(diff failed") {
                let _ = manager.update_run_value(
                    &winner.id,
                    serde_json::json!({ "status": coducktor_contract::RunStatus::Review }),
                );
            }
        }
        let _ = manager.append_event(
            &winner.id,
            lifecycle_event(format!(
                "picked from {} variants — {} other variant(s) archived",
                losers.len() + 1,
                losers.len()
            )),
        );
        append_handoff_heartbeat(
            &data_dir,
            &winner.id,
            &format!("picked from {} variants", losers.len() + 1),
        );
        for loser in losers {
            if manager.is_active(&loser.id) {
                let _ = manager.cancel(&loser.id);
            }
            if let Some(path) = loser.worktree_path.as_deref() {
                coducktor_core::git::worktree::remove_worktree(
                    &repo_root,
                    Path::new(path),
                    loser.branch.as_deref(),
                );
            }
            let _ = manager.update_run_value(
                &loser.id,
                serde_json::json!({ "worktreePath": null, "branch": null }),
            );
            let _ = manager.set_archived(&loser.id, true);
            let _ = manager.append_event(
                &loser.id,
                lifecycle_event(format!(
                    "variant {} was picked — this variant is archived, its worktree removed",
                    winner.variant.as_deref().unwrap_or("?")
                )),
            );
        }
        Ok(PickVariantResponse {
            winner: manager.get_run(&winner.id).cloned(),
        })
    }

    // ---- open-targets (spec §8.7 "open in") -------------------------------------------------
    // Ported from `coducktor-server`'s `list_open_targets`/`open_project_in` handlers,
    // duplicating their private `open_targets`/`open_target_command`/`open_target`/
    // `executable_on_path`/`configured_executable`/`installed_mac_app` helpers byte-for-byte —
    // none were `pub`. `open_project_in` here takes `target: &str` directly (the `Engine`
    // trait's own signature), so there's no `OpenProjectInRequest` JSON body to parse/validate
    // the way the HTTP handler does.

    pub async fn open_targets(&self) -> Result<OpenTargetsResponse, EngineError> {
        tokio::task::spawn_blocking(|| OpenTargetsResponse {
            targets: open_targets_list(),
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn open_project_in(
        &self,
        target: &str,
    ) -> Result<OpenProjectInResponse, EngineError> {
        let target = target.trim();
        if target.is_empty() || target.chars().count() > 200 {
            return Err(EngineError::Conflict {
                reason: "target required".to_owned(),
            });
        }
        if target.starts_with("cli:") {
            return Err(EngineError::Conflict {
                reason: "agent CLIs open a task worktree, not the project folder".to_owned(),
            });
        }
        if !open_targets_list()
            .iter()
            .any(|candidate| candidate.id == target)
        {
            return Err(EngineError::Conflict {
                reason: format!("no such app on this machine: {target}"),
            });
        }
        let repo_root = self.repo_root.clone();
        let target = target.to_owned();
        let target_for_error = target.clone();
        let opened = tokio::task::spawn_blocking(move || open_target(&repo_root, &target))
            .await
            .map_err(|error| EngineError::Transport(error.to_string()))?;
        if !opened {
            return Err(EngineError::Conflict {
                reason: format!("could not open {target_for_error}"),
            });
        }
        Ok(OpenProjectInResponse {
            opened: true,
            path: self.repo_root.to_string_lossy().into_owned(),
        })
    }

    // ---- host model catalog (ported from `coducktor-server`'s `get_models` handler) --------

    /// `GET /models?runner=` — a 5-minute-TTL-cached host-discovered model catalog for
    /// `codex`/`opencode` (`claude`/`pi` have no discovery path and are rejected, matching
    /// `runner_discovers_models`). Live discovery shells out to the real CLI; a failure falls
    /// back to the last good cached catalog (stale-but-something beats nothing).
    pub async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError> {
        let runner = match runner {
            Runner::Codex => ModelDiscoveryRunner::Codex,
            Runner::OpenCode => ModelDiscoveryRunner::OpenCode,
            Runner::Claude | Runner::Pi => {
                return Err(EngineError::Conflict {
                    reason: "runner must be codex or opencode".to_owned(),
                });
            }
        };
        let now = Instant::now();
        if let Ok(cache) = self.model_catalog.lock()
            && let Some(entry) = cache.iter().find(|entry| entry.runner == runner)
            && now < entry.expires_at
        {
            let source = if entry.failure_reason.is_some() && entry.models.is_empty() {
                ModelCatalogSource::Unavailable
            } else {
                ModelCatalogSource::Cache
            };
            return Ok(model_catalog_wire(
                runner,
                entry.models.clone(),
                source,
                entry.failure_reason.is_some() && !entry.models.is_empty(),
                entry.failure_reason.clone(),
            ));
        }

        let discovered = match runner {
            ModelDiscoveryRunner::Codex => discover_codex_models(&self.repo_root).await,
            ModelDiscoveryRunner::OpenCode => discover_opencode_models(&self.repo_root).await,
        };
        let (models, source, stale, reason) = match discovered {
            Ok(models) => (models, ModelCatalogSource::Live, false, None),
            Err(()) => {
                let cached =
                    self.model_catalog.lock().ok().and_then(|cache| {
                        cache.iter().find(|entry| entry.runner == runner).cloned()
                    });
                let models = cached.map(|entry| entry.models).unwrap_or_default();
                let stale = !models.is_empty();
                (
                    models,
                    if stale {
                        ModelCatalogSource::Cache
                    } else {
                        ModelCatalogSource::Unavailable
                    },
                    stale,
                    Some(model_catalog_reason(runner)),
                )
            }
        };
        if let Ok(mut cache) = self.model_catalog.lock() {
            cache.retain(|entry| entry.runner != runner);
            cache.push(CachedModelCatalog {
                runner,
                models: models.clone(),
                expires_at: Instant::now() + MODEL_CATALOG_TTL,
                failure_reason: reason
                    .clone()
                    .filter(|_| source != ModelCatalogSource::Live),
            });
        }
        Ok(model_catalog_wire(runner, models, source, stale, reason))
    }

    // ---- plan (ported from `coducktor-server`'s `create_plan_at` handler) ------------------

    /// `POST /plan` — always the safe single-step fallback plan today (no live planner is
    /// wired), gated only by task-length validation and the default runner's provider not
    /// being disabled in Settings. Matches `fallback_plan`/`plan_provider_disabled` exactly.
    pub async fn plan(&self, task: &str) -> Result<PlanResponse, EngineError> {
        let trimmed = task.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 100_000 {
            return Err(EngineError::Conflict {
                reason: "task must be between 1 and 100000 characters".to_owned(),
            });
        }
        if let Some(reason) = self.plan_provider_disabled() {
            return Err(EngineError::Conflict { reason });
        }
        Ok(fallback_plan())
    }

    fn plan_provider_disabled(&self) -> Option<String> {
        let workspace = workspace_config_for(&self.repo_root);
        let config = load_config(&self.repo_root, &workspace.agent_defaults);
        let provider = match config.default_runner {
            RunnerSelection::Auto => return None,
            RunnerSelection::Claude => Runner::Claude,
            RunnerSelection::Codex => Runner::Codex,
            RunnerSelection::OpenCode => Runner::OpenCode,
            RunnerSelection::Pi => Runner::Pi,
        };
        workspace.disabled_providers.contains(&provider).then(|| {
            format!(
                "{} is disabled. Enable it in Settings → Agents → Providers.",
                provider_label(provider)
            )
        })
    }

    // ---- GitHub forge (ported from `coducktor-server`'s github_* handlers, which delegate to
    // `coducktor-forge`'s `GithubDriver`, B7). Every method's driver resolution and I/O runs
    // inside `spawn_blocking` — `GithubDriver`'s own methods are synchronous (they shell out to
    // `gh`/`git`), matching how this file's other git-shelling families (repo/run git browsing,
    // C1.5) already isolate blocking work from the async executor. ----------------------------

    const GITHUB_UNAVAILABLE_REASON: &str = "GitHub is unavailable for this repository";

    /// Resolve a driver from the repo's `origin` remote — synchronous, run only from inside a
    /// `spawn_blocking` closure.
    fn github_driver_blocking(repo_root: &Path) -> Option<GithubDriver> {
        let remote = git_output(repo_root, &["config", "--get", "remote.origin.url"]);
        resolve_forge(repo_root.to_path_buf(), remote.as_deref())
    }

    fn unavailable_github() -> GithubData {
        GithubData {
            available: false,
            reason: Some(Self::GITHUB_UNAVAILABLE_REASON.to_owned()),
            repo: None,
            synced_at: None,
            issues: Vec::new(),
            prs: Vec::new(),
            label_colors: None,
        }
    }

    pub async fn github(&self) -> Result<GithubData, EngineError> {
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || match Self::github_driver_blocking(&repo_root) {
            Some(driver) => driver.list(false, 30),
            None => Self::unavailable_github(),
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    /// `prs` mirrors the trait's already-parsed `&[String]` — each entry must be a bare positive
    /// integer, matching `parse_github_numbers`'s own validation (the HTTP path parses a
    /// comma-joined query string into the same shape before this point).
    pub async fn github_checks(&self, prs: &[String]) -> Result<GithubChecksData, EngineError> {
        if prs.is_empty() || prs.len() > 100 {
            return Err(EngineError::Conflict {
                reason: "invalid prs query".to_owned(),
            });
        }
        let numbers: Option<Vec<u64>> = prs
            .iter()
            .map(|value| value.parse::<u64>().ok().filter(|number| *number > 0))
            .collect();
        let Some(numbers) = numbers else {
            return Err(EngineError::Conflict {
                reason: "invalid prs query".to_owned(),
            });
        };
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            let Some(driver) = Self::github_driver_blocking(&repo_root) else {
                return GithubChecksData::Unavailable(GithubChecksUnavailable {
                    available: false,
                    reason: Self::GITHUB_UNAVAILABLE_REASON.to_owned(),
                });
            };
            match driver.checks(&numbers) {
                Ok(checks) => GithubChecksData::Available(GithubChecksAvailable {
                    available: true,
                    checks: checks
                        .into_iter()
                        .map(|(number, glyph)| (number.to_string(), glyph))
                        .collect(),
                }),
                Err(reason) => GithubChecksData::Unavailable(GithubChecksUnavailable {
                    available: false,
                    reason,
                }),
            }
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn github_ref_status(
        &self,
        prs: &[String],
        issues: &[String],
    ) -> Result<GithubRefStatusData, EngineError> {
        if prs.len() > 100 || issues.len() > 100 || (prs.is_empty() && issues.is_empty()) {
            return Err(EngineError::Conflict {
                reason: if prs.is_empty() && issues.is_empty() {
                    "missing prs or issues query".to_owned()
                } else {
                    "invalid ref-status query".to_owned()
                },
            });
        }
        let parse_numbers = |values: &[String]| {
            values
                .iter()
                .map(|value| {
                    let number = value.parse::<u64>().ok().filter(|number| *number > 0)?;
                    (number.to_string() == *value).then_some(number)
                })
                .collect::<Option<Vec<_>>>()
        };
        let Some(prs) = parse_numbers(prs) else {
            return Err(EngineError::Conflict {
                reason: "invalid ref-status query".to_owned(),
            });
        };
        let Some(issues) = parse_numbers(issues) else {
            return Err(EngineError::Conflict {
                reason: "invalid ref-status query".to_owned(),
            });
        };
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            let Some(driver) = Self::github_driver_blocking(&repo_root) else {
                return GithubRefStatusData::Unavailable(GithubRefStatusUnavailable {
                    available: false,
                    reason: Self::GITHUB_UNAVAILABLE_REASON.to_owned(),
                    recheck_after_ms: None,
                });
            };
            let status = driver.ref_status(&prs, &issues);
            if !status.available {
                return GithubRefStatusData::Unavailable(GithubRefStatusUnavailable {
                    available: false,
                    reason: status
                        .reason
                        .unwrap_or_else(|| Self::GITHUB_UNAVAILABLE_REASON.to_owned()),
                    recheck_after_ms: status.recheck_after_ms.map(|value| value as f64),
                });
            }
            GithubRefStatusData::Available(GithubRefStatusAvailable {
                available: true,
                prs: status
                    .prs
                    .into_iter()
                    .map(|(number, value)| (number.to_string(), value))
                    .collect(),
                issues: status
                    .issues
                    .into_iter()
                    .map(|(number, value)| (number.to_string(), value))
                    .collect(),
                recheck_after_ms: status.recheck_after_ms.map(|value| value as f64),
            })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn github_comments(
        &self,
        kind: &str,
        number: u64,
    ) -> Result<GithubCommentsData, EngineError> {
        let kind = match kind {
            "issue" => GithubItemKind::Issue,
            "pr" => GithubItemKind::Pr,
            _ => {
                return Err(EngineError::Conflict {
                    reason: "invalid kind or number".to_owned(),
                });
            }
        };
        if number == 0 {
            return Err(EngineError::Conflict {
                reason: "invalid kind or number".to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            Self::github_driver_blocking(&repo_root)
                .map(|driver| driver.comments(kind, number, false))
                .unwrap_or_else(|| GithubCommentsData {
                    available: false,
                    reason: Some(Self::GITHUB_UNAVAILABLE_REASON.to_owned()),
                    comments: Vec::new(),
                    truncated: None,
                    events: None,
                })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    pub async fn github_pr_merge_state(
        &self,
        number: u64,
    ) -> Result<GithubPrMergeStateResponse, EngineError> {
        if number == 0 {
            return Err(EngineError::Conflict {
                reason: "invalid pull request number".to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            let Some(driver) = Self::github_driver_blocking(&repo_root) else {
                return GithubPrMergeStateResponse::Unavailable {
                    available: false,
                    reason: Self::GITHUB_UNAVAILABLE_REASON.to_owned(),
                };
            };
            match driver.pr_merge_state(number, false) {
                ForgePrMergeStateResult::Available(state) => {
                    GithubPrMergeStateResponse::Available {
                        available: true,
                        merge_state: state,
                    }
                }
                ForgePrMergeStateResult::Unavailable { reason } => {
                    GithubPrMergeStateResponse::Unavailable {
                        available: false,
                        reason,
                    }
                }
            }
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    /// The rejection branch of `coducktor-server`'s `merge_github_pr_for` carries a dynamic HTTP
    /// status, an optional machine-readable `code`, and an optional `current` merge-state
    /// snapshot in its JSON body — [`EngineError::Conflict`] only has room for a `reason` string,
    /// so `code`/`current` are dropped here. A real, named reduction (same category as this
    /// module's other documented cuts), not an oversight.
    pub async fn github_merge_pr(
        &self,
        number: u64,
        input: &GithubMergeInput,
    ) -> Result<GithubMergeResponse, EngineError> {
        if number == 0 {
            return Err(EngineError::Conflict {
                reason: "invalid pull request number".to_owned(),
            });
        }
        if input.expected_head_sha.len() != 40
            || !input
                .expected_head_sha
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        {
            return Err(EngineError::Conflict {
                reason: "invalid merge request".to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        let input = ForgeMergeInput {
            method: input.method,
            expected_head_sha: input.expected_head_sha.clone(),
            override_rules: input.override_rules.unwrap_or(false),
        };
        tokio::task::spawn_blocking(move || {
            let Some(driver) = Self::github_driver_blocking(&repo_root) else {
                return Err(EngineError::Conflict {
                    reason: "GitHub merge is unavailable".to_owned(),
                });
            };
            match driver.merge_pr(number, &input) {
                ForgeMergeResult::Merged {
                    number,
                    url,
                    method,
                    merge_commit_sha,
                } => Ok(GithubMergeResponse {
                    merged: true,
                    number,
                    url,
                    method,
                    merge_commit_sha,
                }),
                ForgeMergeResult::Rejected { error, .. } => {
                    Err(EngineError::Conflict { reason: error })
                }
            }
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn github_pr_changes(&self, number: u64) -> Result<GithubPrChangesData, EngineError> {
        if number == 0 {
            return Err(EngineError::Conflict {
                reason: "invalid pull request number or refresh flag".to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        tokio::task::spawn_blocking(move || {
            let Some(driver) = Self::github_driver_blocking(&repo_root) else {
                return GithubPrChangesData::Unavailable(GithubPrChangesUnavailable {
                    available: false,
                    reason: Self::GITHUB_UNAVAILABLE_REASON.to_owned(),
                });
            };
            match driver.pr_diff(number, false) {
                ForgePrDiffResult::Available {
                    number,
                    head_sha,
                    files,
                    additions,
                    deletions,
                    truncated,
                    reason,
                } => GithubPrChangesData::Available(GithubPrChangesAvailable {
                    available: true,
                    number,
                    head_sha,
                    files: files
                        .into_iter()
                        .map(|file| coducktor_contract::GithubPrChange {
                            path: file.path,
                            previous_path: file.previous_path,
                            status: file.status,
                            additions: file.additions,
                            deletions: file.deletions,
                            patch: file.patch,
                            patch_unavailable_reason: file.patch_unavailable_reason,
                            truncated: file.truncated.then_some(true),
                        })
                        .collect(),
                    additions,
                    deletions,
                    truncated,
                    reason,
                }),
                ForgePrDiffResult::Unavailable { reason } => {
                    GithubPrChangesData::Unavailable(GithubPrChangesUnavailable {
                        available: false,
                        reason,
                    })
                }
            }
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))
    }

    // ---- remaining settings writes (ported from `coducktor-server`'s workspace_config/
    // workspace_ui_state/remove_project/update_project handlers) ----------------------------

    pub async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError> {
        Ok(workspace_config_response(&workspace_config_for(
            &self.repo_root,
        )))
    }

    pub async fn put_workspace_config(
        &self,
        input: &SetWorkspaceConfigInput,
    ) -> Result<WorkspaceConfigResponse, EngineError> {
        validate_workspace_config_input(input)
            .map_err(|reason| EngineError::Conflict { reason })?;
        let path = coducktor_core::paths::workspace_config_path(&ProcessEnv);
        let input = input.clone();
        let saved = merge_write_workspace_config(&path, &ProcessEnv, |config| {
            apply_workspace_config_input(config, &input);
        })
        .map_err(io_err)?;
        Ok(workspace_config_response(&saved))
    }

    pub async fn workspace_ui_state(&self) -> Result<WorkspaceUiState, EngineError> {
        let path = coducktor_core::paths::workspace_ui_state_path(&ProcessEnv);
        Ok(read_workspace_ui_state(&path))
    }

    pub async fn put_workspace_ui_state(
        &self,
        input: &SetWorkspaceUiStateInput,
    ) -> Result<WorkspaceUiState, EngineError> {
        let path = coducktor_core::paths::workspace_ui_state_path(&ProcessEnv);
        let input = input.clone();
        merge_write_workspace_ui_state(&path, |state| {
            if input.sidebar.is_some() {
                state.sidebar = input.sidebar.clone();
            }
            if input.dismissed_provider_auth_failures.is_some() {
                state.dismissed_provider_auth_failures =
                    input.dismissed_provider_auth_failures.clone();
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
            state.extra.extend(input.extra.clone());
        })
        .map_err(io_err)
    }

    pub async fn remove_project(
        &self,
        project_id: &str,
    ) -> Result<RemoveProjectResponse, EngineError> {
        let config_path = coducktor_core::paths::workspace_config_path(&ProcessEnv);
        let config = load_workspace_config(&config_path, &ProcessEnv);
        let boot_id = boot_project_id(&config, &self.repo_root);
        let id = if project_id == "default" {
            boot_id.clone()
        } else {
            project_id.to_owned()
        };
        if !config.projects.iter().any(|project| project.id == id) {
            return Err(EngineError::NotFound);
        }
        if id == boot_id {
            return Err(EngineError::Conflict {
                reason: "cannot remove the boot project".to_owned(),
            });
        }
        let removed_id = id.clone();
        merge_write_workspace_config(&config_path, &ProcessEnv, move |config| {
            config.projects.retain(|project| project.id != id);
        })
        .map_err(io_err)?;
        Ok(RemoveProjectResponse {
            removed: true,
            id: removed_id,
        })
    }

    pub async fn update_project(
        &self,
        project_id: &str,
        input: &UpdateProjectInput,
    ) -> Result<UpdateProjectResponse, EngineError> {
        validate_project_update(input).map_err(|reason| EngineError::Conflict { reason })?;
        let config_path = coducktor_core::paths::workspace_config_path(&ProcessEnv);
        let config = load_workspace_config(&config_path, &ProcessEnv);
        let boot_id = boot_project_id(&config, &self.repo_root);
        let id = if project_id == "default" {
            boot_id
        } else {
            project_id.to_owned()
        };
        if !config.projects.iter().any(|project| project.id == id) {
            return Err(EngineError::NotFound);
        }
        let max_parallel = input.max_parallel;
        let tags = input.tags.clone();
        let target_id = id.clone();
        let mut updated = None;
        merge_write_workspace_config(&config_path, &ProcessEnv, |config| {
            if let Some(project) = config
                .projects
                .iter_mut()
                .find(|project| project.id == target_id)
            {
                if let Some(value) = max_parallel {
                    project.max_parallel = value;
                }
                if let Some(value) = tags.clone() {
                    project.tags = normalize_project_tags(value);
                }
                updated = Some(project.clone());
            }
        })
        .map_err(io_err)?;
        let Some(updated) = updated else {
            return Err(EngineError::NotFound);
        };
        Ok(UpdateProjectResponse {
            project: project_entry(&updated),
        })
    }

    // ---- task-thread write paths (ported from `coducktor-server`'s send_message/
    // edit_queued_message/remove_queued_message/continue_run/cancel_auto_resume/
    // run_git_commit/run_git_push/run_commits/run_pr/run_history/run_history_context/
    // open_run_in_cli/open_run_in handlers) ---------------------------------------------

    pub async fn send_message(
        &self,
        run_id: &str,
        input: MessageInput,
    ) -> Result<MessageResponse, EngineError> {
        let Some(text) = input.text.filter(|value| !value.trim().is_empty()) else {
            return Err(EngineError::Conflict {
                reason: "message needs text or at least one image".to_owned(),
            });
        };
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        match manager.send_message(run_id, text) {
            Ok(true) => Ok(MessageResponse::Delivered { delivered: true }),
            Ok(false) => Err(EngineError::Conflict {
                reason: "session closed".to_owned(),
            }),
            Err(error) => Err(io_err(error)),
        }
    }

    pub async fn continue_run(
        &self,
        run_id: &str,
        input: ContinueInput,
    ) -> Result<ContinueResponse, EngineError> {
        let options = coducktor_core::workflows::run::ContinueOptions {
            text: input.text,
            runner: input.runner,
            model: input.model,
        };
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        match manager.continue_run(run_id, options) {
            Ok(result) if result.ok => Ok(ContinueResponse { continued: true }),
            Ok(result) => Err(EngineError::Conflict {
                reason: result
                    .error
                    .unwrap_or_else(|| "cannot continue run".to_owned()),
            }),
            Err(error) => Err(io_err(error)),
        }
    }

    pub async fn edit_queued_message(
        &self,
        run_id: &str,
        message_id: &str,
        input: QueuedMessagePatchInput,
    ) -> Result<EditQueuedMessageResponse, EngineError> {
        let run = self.run_record(run_id)?;
        if input.text.is_none() && input.images.is_none() {
            return Err(EngineError::Conflict {
                reason: "message edit needs text or images".to_owned(),
            });
        }
        if input
            .text
            .as_deref()
            .is_some_and(|text| !valid_queued_text(text))
            || input.images.as_ref().is_some_and(|images| images.len() > 4)
        {
            return Err(EngineError::Conflict {
                reason: "queued message exceeds its limits".to_owned(),
            });
        }
        let Some(stack) = run.queued_messages.clone() else {
            return Err(EngineError::NotFound);
        };
        let Some(current) = stack.iter().find(|message| message.id == message_id) else {
            return Err(EngineError::NotFound);
        };
        if run.status != coducktor_contract::RunStatus::Queued {
            return Err(EngineError::Conflict {
                reason: "run already started".to_owned(),
            });
        }
        let text = input.text.clone().unwrap_or_else(|| current.text.clone());
        let images = input
            .images
            .as_deref()
            .map(image_input_urls)
            .or_else(|| current.images.clone());
        let effective_images = images.as_ref().map_or(0, Vec::len);
        let other_images = stack
            .iter()
            .filter(|message| message.id != message_id)
            .map(|message| message.images.as_ref().map_or(0, Vec::len))
            .sum::<usize>();
        if text.trim().is_empty() && effective_images == 0 {
            return Err(EngineError::Conflict {
                reason: "message needs text or at least one image".to_owned(),
            });
        }
        if other_images + effective_images > MAX_QUEUED_IMAGES {
            return Err(EngineError::Conflict {
                reason: "too many queued images — 8 image limit across the stack".to_owned(),
            });
        }
        let mut prospective = stack.clone();
        if let Some(message) = prospective
            .iter_mut()
            .find(|message| message.id == message_id)
        {
            message.text = text.clone();
            message.images = images.clone().filter(|images| !images.is_empty());
        }
        if folded_task_length(&run.task, &prospective) > MAX_FOLDED_TASK_CHARS {
            return Err(EngineError::Conflict {
                reason: "prompt too long — 200000 character limit across the task and its queued messages"
                    .to_owned(),
            });
        }
        let replacement = prospective
            .iter()
            .find(|message| message.id == message_id)
            .cloned();
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager
            .edit_run(run_id, |record| record.queued_messages = Some(prospective))
            .ok()
            .flatten()
            .is_none()
        {
            return Err(EngineError::Conflict {
                reason: "run already started".to_owned(),
            });
        }
        Ok(EditQueuedMessageResponse {
            message: replacement.unwrap_or_else(|| current.clone()),
        })
    }

    pub async fn remove_queued_message(
        &self,
        run_id: &str,
        message_id: &str,
    ) -> Result<RemoveQueuedMessageResponse, EngineError> {
        let run = self.run_record(run_id)?;
        if !run
            .queued_messages
            .as_ref()
            .is_some_and(|messages| messages.iter().any(|message| message.id == message_id))
        {
            return Err(EngineError::NotFound);
        }
        if run.status != coducktor_contract::RunStatus::Queued {
            return Err(EngineError::Conflict {
                reason: "run already started".to_owned(),
            });
        }
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        let updated = manager
            .edit_run(run_id, |record| {
                if let Some(messages) = record.queued_messages.as_mut() {
                    messages.retain(|message| message.id != message_id);
                }
            })
            .ok()
            .flatten();
        if updated.is_none() {
            return Err(EngineError::Conflict {
                reason: "run already started".to_owned(),
            });
        }
        Ok(RemoveQueuedMessageResponse { removed: true })
    }

    pub async fn cancel_auto_resume(
        &self,
        run_id: &str,
    ) -> Result<CancelAutoResumeResponse, EngineError> {
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        if manager.get_run(run_id).is_none() {
            return Err(EngineError::NotFound);
        }
        let mut patch = Map::new();
        patch.insert("autoResumeAt".to_owned(), Value::Null);
        patch.insert("autoResumeAttempts".to_owned(), Value::Null);
        match manager.update_run_value(run_id, Value::Object(patch)) {
            Ok(Some(_)) => Ok(CancelAutoResumeResponse { cancelled: true }),
            Ok(None) => Err(EngineError::NotFound),
            Err(error) => Err(io_err(error)),
        }
    }

    pub async fn git_commit(
        &self,
        run_id: &str,
        input: GitCommitInput,
    ) -> Result<GitCommitResponse, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(worktree) = run_worktree_of(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        tokio::task::spawn_blocking(move || {
            commit_all(&worktree, &input.message)
                .map(|sha| GitCommitResponse {
                    committed: true,
                    sha,
                })
                .map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn git_push(&self, run_id: &str) -> Result<GitPushResponse, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(worktree) = run_worktree_of(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        tokio::task::spawn_blocking(move || {
            push_current_branch(&worktree).map_err(|reason| EngineError::Conflict { reason })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    pub async fn run_commits(&self, run_id: &str) -> Result<RunCommitsResponse, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(root) = self.working_directory_of(&run) else {
            return Err(EngineError::Conflict {
                reason: NO_WORKTREE.to_owned(),
            });
        };
        let base = run.base_branch.clone().unwrap_or_else(|| "HEAD".to_owned());
        tokio::task::spawn_blocking(move || {
            let commits = collect_run_commits(&root, &base)
                .map_err(|reason| EngineError::Conflict { reason })?;
            let (current_branch, pushed) = run_git_status(&root);
            Ok(RunCommitsResponse {
                commits,
                branch: run.branch.or(current_branch),
                pushed,
            })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?
    }

    fn working_directory_of(&self, run: &coducktor_contract::RunRecord) -> Option<PathBuf> {
        if run.worktree == Some(false) {
            Some(self.repo_root.clone())
        } else {
            run_worktree_of(run)
        }
    }

    /// `create_pr` (`POST /runs/:id/pr`, `coducktor-server`'s `run_pr` handler) — publishes a
    /// draft PR via `coducktor-forge`'s `create_draft_pr` and records the outcome on the run.
    pub async fn create_pr(&self, run_id: &str) -> Result<CreatePrResponse, EngineError> {
        let run = self.run_record(run_id)?;
        {
            let manager = self.manager.lock().map_err(|_| lock_err())?;
            if manager.is_active(run_id) {
                return Err(EngineError::Conflict {
                    reason: "run is still active — wait for the review gate".to_owned(),
                });
            }
        }
        if run_worktree_of(&run).is_none() || run.branch.is_none() {
            return Err(EngineError::Conflict {
                reason: "no worktree/branch to publish — this task ran in the repo working tree"
                    .to_owned(),
            });
        }
        let repo_root = self.repo_root.clone();
        let handoff_text = read_handoff(&data_dir(&self.repo_root), run_id);
        let outcome = tokio::task::spawn_blocking(move || {
            Self::github_driver_blocking(&repo_root).map(|driver| {
                driver.create_draft_pr(&DraftPrInput {
                    repo_root,
                    run: run.clone(),
                    handoff_text,
                })
            })
        })
        .await
        .map_err(|error| EngineError::Transport(error.to_string()))?;
        let Some(outcome) = outcome else {
            return Err(EngineError::Conflict {
                reason: "no GitHub forge configured for this repository".to_owned(),
            });
        };
        let (url, dry_run) = match outcome {
            DraftPrOutcome::Created { url, dry_run } => (url, dry_run),
            DraftPrOutcome::Failed { error } => {
                return Err(EngineError::Conflict { reason: error });
            }
        };
        let run = self.run_record(run_id)?;
        let finished_at = run
            .finished_at
            .unwrap_or_else(coducktor_core::time::now_iso8601);
        let mut manager = self.manager.lock().map_err(|_| lock_err())?;
        manager
            .update_run_value(
                run_id,
                json!({ "pullRequestUrl": url, "status": "done", "finishedAt": finished_at }),
            )
            .map_err(|_| EngineError::Transport("could not update run".to_owned()))?;
        let _ = manager.append_event(
            run_id,
            EventInput::new("note").field(
                "message",
                format!(
                    "draft PR created: {url}{}",
                    if dry_run {
                        " (dry run — no real PR)"
                    } else {
                        ""
                    }
                ),
            ),
        );
        Ok(CreatePrResponse { url, dry_run })
    }

    pub async fn run_history(
        &self,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError> {
        if self
            .manager
            .lock()
            .map_err(|_| lock_err())?
            .get_run(run_id)
            .is_none()
        {
            return Err(EngineError::NotFound);
        }
        self.read_history_page(run_id, cursor)
    }

    pub async fn run_history_context(
        &self,
        run_id: &str,
    ) -> Result<RunHistoryContext, EngineError> {
        if self
            .manager
            .lock()
            .map_err(|_| lock_err())?
            .get_run(run_id)
            .is_none()
        {
            return Err(EngineError::NotFound);
        }
        let events = self
            .manager
            .lock()
            .map_err(|_| lock_err())?
            .read_events(run_id);
        let mut latest_plan = None;
        let mut selected = BTreeMap::new();
        for event in events.iter() {
            if event.event_type == "plan.updated"
                || (event.event_type == "tool-call"
                    && event.extra.get("tool").and_then(Value::as_str) == Some("TodoWrite"))
            {
                latest_plan = Some(event.clone());
                continue;
            }
            if is_history_boundary(event)
                || matches!(
                    event.event_type.as_str(),
                    "turn.completed" | "session.ended" | "session.error"
                )
            {
                selected.insert(event_seq_u64(event.seq), event.clone());
                continue;
            }
            if matches!(
                event.event_type.as_str(),
                "item.started" | "item.updated" | "item.completed"
            ) && event
                .extra
                .get("item")
                .and_then(Value::as_object)
                .and_then(|item| item.get("kind"))
                .and_then(Value::as_str)
                == Some("tool")
            {
                selected.insert(event_seq_u64(event.seq), event.clone());
            }
        }
        if let Some(event) = latest_plan {
            selected.insert(event_seq_u64(event.seq), event);
        }
        Ok(RunHistoryContext {
            context_events: selected.into_values().map(history_event).collect(),
            as_of_seq: events
                .iter()
                .map(|event| event_seq_u64(event.seq))
                .max()
                .unwrap_or(0),
        })
    }

    fn run_events_path(&self, run_id: &str) -> PathBuf {
        data_dir(&self.repo_root)
            .join("runs")
            .join(format!("{run_id}.ndjson"))
    }

    fn read_history_page(
        &self,
        run_id: &str,
        cursor: Option<&str>,
    ) -> Result<RunHistoryPage, EngineError> {
        let path = self.run_events_path(run_id);
        let file_size = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let decoded = cursor.map(decode_cursor::<PageCursor>).transpose()?;
        if let Some(decoded) = &decoded
            && (decoded.v != 1
                || decoded.kind != "page"
                || !matches!(decoded.direction.as_str(), "older" | "newer"))
        {
            return Err(EngineError::Conflict {
                reason: "invalid history cursor".to_owned(),
            });
        }
        if decoded
            .as_ref()
            .is_some_and(|value| value.file_size > file_size)
        {
            return Err(EngineError::Conflict {
                reason: "history cursor is no longer valid — reload the newest page".to_owned(),
            });
        }

        let events = self
            .manager
            .lock()
            .map_err(|_| lock_err())?
            .read_events(run_id);
        let mut units: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| (!is_history_boundary(event)).then_some(index))
            .collect();
        if units.is_empty() {
            units = (0..events.len()).collect();
        }

        let selected: Vec<usize> = match decoded.as_ref().map(|value| value.direction.as_str()) {
            Some("older") => units
                .iter()
                .copied()
                .filter(|index| {
                    events[*index].seq
                        < decoded
                            .as_ref()
                            .map_or(0.0, |value| value.boundary_seq as f64)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .take(RUN_HISTORY_PAGE_ITEMS as usize)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            Some("newer") => units
                .iter()
                .copied()
                .filter(|index| {
                    events[*index].seq
                        > decoded
                            .as_ref()
                            .map_or(0.0, |value| value.boundary_seq as f64)
                })
                .take(RUN_HISTORY_PAGE_ITEMS as usize)
                .collect(),
            _ => units
                .iter()
                .copied()
                .rev()
                .take(RUN_HISTORY_PAGE_ITEMS as usize)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        };

        let (page_events, first_seq, last_seq, item_count) =
            if let (Some(first), Some(last)) = (selected.first(), selected.last()) {
                let mut start = *first;
                if let Some(boundary) = events[..start].iter().rposition(is_history_boundary) {
                    start = boundary;
                }
                let page = events[start..=*last].to_vec();
                (
                    page,
                    events[*first].seq,
                    events[*last].seq,
                    selected.len() as u64,
                )
            } else {
                (Vec::new(), 0.0, 0.0, 0)
            };
        let has_older = selected.first().is_some_and(|first| {
            units
                .iter()
                .any(|index| events[*index].seq < events[*first].seq)
        });
        let has_newer = selected.last().is_some_and(|last| {
            units
                .iter()
                .any(|index| events[*index].seq > events[*last].seq)
        });
        let as_of_seq = events
            .iter()
            .map(|event| event_seq_u64(event.seq))
            .max()
            .unwrap_or(0);
        let live_cursor = encode_cursor(&json!({
            "v": 1,
            "kind": "live",
            "offset": file_size,
            "boundarySeq": as_of_seq,
        }));
        let older_cursor = has_older.then(|| {
            encode_cursor(&PageCursor {
                v: 1,
                kind: "page".to_owned(),
                direction: "older".to_owned(),
                file_size,
                boundary_seq: event_seq_u64(first_seq),
            })
        });
        let newer_cursor = has_newer.then(|| {
            encode_cursor(&PageCursor {
                v: 1,
                kind: "page".to_owned(),
                direction: "newer".to_owned(),
                file_size,
                boundary_seq: event_seq_u64(last_seq),
            })
        });
        Ok(RunHistoryPage {
            events: page_events.into_iter().map(history_event).collect(),
            item_count,
            older_cursor,
            newer_cursor,
            live_cursor,
            as_of_seq,
            has_older,
        })
    }

    pub async fn open_in_cli(&self, run_id: &str) -> Result<OpenInCliResponse, EngineError> {
        let run = self.run_record(run_id)?;
        let Some(command) = run_resume_command(&run) else {
            return Err(EngineError::Conflict {
                reason: "no agent session to resume".to_owned(),
            });
        };
        let directory = run_worktree_of(&run).unwrap_or_else(|| self.repo_root.clone());
        let launch = format!(
            "cd {} && {command}",
            shell_quote(&directory.to_string_lossy())
        );
        if !open_terminal_for_command(&launch) {
            return Err(EngineError::Conflict {
                reason: "no terminal emulator found".to_owned(),
            });
        }
        Ok(OpenInCliResponse {
            opened: true,
            command,
        })
    }

    pub async fn open_in(&self, run_id: &str, input: OpenInInput) -> Result<Value, EngineError> {
        let run = self.run_record(run_id)?;
        let target = input.target.trim();
        if target.is_empty() || target.chars().count() > 200 {
            return Err(EngineError::Conflict {
                reason: "target required".to_owned(),
            });
        }
        let directory = run_worktree_of(&run).unwrap_or_else(|| self.repo_root.clone());
        if target == "default" {
            let Some(worktree) = run_worktree_of(&run) else {
                return Err(EngineError::Conflict {
                    reason: NO_WORKTREE.to_owned(),
                });
            };
            let Some(path) = input.path.as_deref().filter(|path| !path.is_empty()) else {
                return Err(EngineError::Conflict {
                    reason: "path required for the default-app target".to_owned(),
                });
            };
            let Ok(WorktreeEntry::File { path, .. }) = read_worktree_path(&worktree, path) else {
                return Err(EngineError::Conflict {
                    reason: "path is not a file in the worktree".to_owned(),
                });
            };
            let file = worktree.join(path);
            if !account_open_default(&file) {
                return Err(EngineError::Conflict {
                    reason: "could not open file".to_owned(),
                });
            }
            return Ok(json!({ "opened": true, "path": file }));
        }
        if let Some(provider) = target.strip_prefix("cli:") {
            let command = match provider {
                "claude" => "claude",
                "codex" => "codex",
                "opencode" => "opencode",
                "pi" => "pi",
                _ => {
                    return Err(EngineError::Conflict {
                        reason: "unknown target".to_owned(),
                    });
                }
            };
            let launch = format!(
                "cd {} && {command}",
                shell_quote(&directory.to_string_lossy())
            );
            if !open_terminal_for_command(&launch) {
                return Err(EngineError::Conflict {
                    reason: "no terminal emulator found".to_owned(),
                });
            }
            return Ok(json!({ "opened": true, "path": directory, "command": command }));
        }
        if !open_targets_list()
            .iter()
            .any(|candidate| candidate.id == target)
        {
            return Err(EngineError::Conflict {
                reason: "unknown target".to_owned(),
            });
        }
        if !open_target(&directory, target) {
            return Err(EngineError::Conflict {
                reason: format!("could not open {target}"),
            });
        }
        Ok(json!({ "opened": true, "path": directory }))
    }
}

const MAX_QUEUED_IMAGES: usize = 8;
const MAX_FOLDED_TASK_CHARS: usize = 200_000;

fn image_input_urls(images: &[ImageInput]) -> Vec<String> {
    images
        .iter()
        .map(|image| format!("data:{};base64,{}", image.media_type, image.data))
        .collect()
}

fn folded_task_length(task: &str, messages: &[coducktor_contract::QueuedMessage]) -> usize {
    std::iter::once(task)
        .chain(messages.iter().map(|message| message.text.as_str()))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
        .len()
}

fn valid_queued_text(text: &str) -> bool {
    text.chars().count() <= 100_000
}

fn commit_all(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("commit message is required".to_owned());
    }
    let status = git_capture(root, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Err("nothing to commit — the working tree is clean".to_owned());
    }
    git_capture(root, &["add", "-A"])?;
    git_capture_owned(
        root,
        &["commit".to_owned(), "-m".to_owned(), message.to_owned()],
    )?;
    git_capture(root, &["rev-parse", "HEAD"]).map(|sha| sha.trim().to_owned())
}

fn push_current_branch(root: &Path) -> Result<GitPushResponse, String> {
    let branch = git_capture(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_owned();
    if branch.is_empty() || branch == "HEAD" {
        return Err("detached HEAD — check out a branch before pushing".to_owned());
    }
    let remotes = git_capture(root, &["remote"])?;
    let remote = remotes
        .lines()
        .map(str::trim)
        .find(|remote| *remote == "origin")
        .or_else(|| {
            remotes
                .lines()
                .map(str::trim)
                .find(|remote| !remote.is_empty())
        })
        .ok_or_else(|| {
            "no remote configured — add one with `git remote add origin <url>`".to_owned()
        })?;
    let upstream = git_capture(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok();
    let push_args = if upstream {
        vec!["push".to_owned()]
    } else {
        vec![
            "push".to_owned(),
            "-u".to_owned(),
            remote.to_owned(),
            branch.clone(),
        ]
    };
    git_capture_owned(root, &push_args)?;
    Ok(GitPushResponse {
        pushed: true,
        branch,
        remote: remote.to_owned(),
        upstream_set: !upstream,
    })
}

fn run_git_status(root: &Path) -> (Option<String>, bool) {
    let branch = git_capture(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(branch) = branch.clone() else {
        return (None, false);
    };
    let remote_refs = git_capture(
        root,
        &[
            "for-each-ref",
            "--contains",
            "HEAD",
            "--format=%(refname)",
            "refs/remotes/",
        ],
    )
    .ok()
    .is_some_and(|value| !value.trim().is_empty());
    if remote_refs {
        return (Some(branch), true);
    }
    let upstream = git_capture(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    );
    if upstream.is_err() {
        return (Some(branch), false);
    }
    let ahead = git_capture(root, &["rev-list", "--count", "@{u}..HEAD"])
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    (Some(branch), ahead == Some(0))
}

fn collect_run_commits(root: &Path, base: &str) -> Result<Vec<RunCommit>, String> {
    if !coducktor_core::git::refs::is_safe_git_ref(base) {
        return Err("refusing option-like base ref".to_owned());
    }
    let base = git_capture(root, &["merge-base", base, "HEAD"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| base.to_owned());
    let revision = format!("{base}..HEAD");
    let log = git_capture(
        root,
        &["log", "--pretty=format:%H%x1f%s%x1f%an%x1f%cr", &revision],
    )?;
    Ok(log
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields = line.split('\x1f').collect::<Vec<_>>();
            RunCommit {
                sha: fields.first().copied().unwrap_or_default().to_owned(),
                subject: fields.get(1).copied().unwrap_or_default().to_owned(),
                author: fields.get(2).copied().unwrap_or_default().to_owned(),
                when: fields.get(3).copied().unwrap_or_default().to_owned(),
            }
        })
        .collect())
}

fn run_resume_command(run: &coducktor_contract::RunRecord) -> Option<String> {
    let session_id = run
        .steps
        .iter()
        .rev()
        .find_map(|step| step.session_id.as_deref())?;
    if !safe_session_id(session_id) {
        return None;
    }
    Some(match run.runner {
        Some(Runner::Codex) => format!("codex resume {session_id}"),
        Some(Runner::OpenCode) => format!("opencode --session {session_id}"),
        Some(Runner::Pi) => format!("pi --session {session_id}"),
        Some(Runner::Claude) | None => format!("claude --resume {session_id}"),
    })
}

fn safe_session_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 200
        && (first.is_ascii_alphanumeric() || matches!(first, '.' | '_'))
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn open_terminal_for_command(command: &str) -> bool {
    if cfg!(target_os = "linux") {
        return Command::new("x-terminal-emulator")
            .args(["-e", "sh", "-lc", command])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
    }
    false
}

fn is_history_boundary(event: &RunEvent) -> bool {
    matches!(event.event_type.as_str(), "user-message" | "turn.started")
}

fn event_seq_u64(seq: f64) -> u64 {
    if seq.is_finite() && seq >= 0.0 && seq <= u64::MAX as f64 {
        seq as u64
    } else {
        0
    }
}

fn history_event(event: RunEvent) -> RunHistoryEvent {
    RunHistoryEvent {
        seq: event.seq,
        ts: event.ts,
        step_id: event.step_id,
        event_type: event.event_type,
        extra: event.extra,
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageCursor {
    v: u8,
    kind: String,
    direction: String,
    file_size: u64,
    boundary_seq: u64,
}

fn decode_cursor<T: serde::de::DeserializeOwned>(cursor: &str) -> Result<T, EngineError> {
    use base64::Engine as _;
    let invalid = || EngineError::Conflict {
        reason: "invalid history cursor".to_owned(),
    };
    if cursor.is_empty() || cursor.len() > 2_048 {
        return Err(invalid());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn encode_cursor<T: Serialize>(value: &T) -> String {
    use base64::Engine as _;
    serde_json::to_vec(value).map_or_else(
        |_| String::new(),
        |bytes| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
    )
}

fn workspace_config_response(
    config: &coducktor_core::workspace::config::WorkspaceConfig,
) -> WorkspaceConfigResponse {
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

fn apply_workspace_config_input(
    config: &mut coducktor_core::workspace::config::WorkspaceConfig,
    input: &SetWorkspaceConfigInput,
) {
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

fn fallback_plan() -> PlanResponse {
    PlanResponse {
        name: None,
        steps: vec![WorkflowStepDef {
            id: "task".to_owned(),
            name: Some("Do the task".to_owned()),
            prompt: Some("{{task}}".to_owned()),
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        }],
        rationale: "planner unavailable — single-step plan".to_owned(),
        fallback: true,
    }
}

fn provider_label(provider: Runner) -> &'static str {
    match provider {
        Runner::Claude => "Claude Code",
        Runner::Codex => "Codex",
        Runner::OpenCode => "OpenCode",
        Runner::Pi => "pi",
    }
}

fn model_catalog_reason(runner: ModelDiscoveryRunner) -> String {
    match runner {
        ModelDiscoveryRunner::Codex => {
            "Codex model discovery is temporarily unavailable".to_owned()
        }
        ModelDiscoveryRunner::OpenCode => {
            "OpenCode model discovery is temporarily unavailable".to_owned()
        }
    }
}

fn model_catalog_wire(
    runner: ModelDiscoveryRunner,
    models: Vec<RunnerModelOption>,
    source: ModelCatalogSource,
    stale: bool,
    reason: Option<String>,
) -> RunnerModelCatalogResponse {
    RunnerModelCatalogResponse {
        runner: match runner {
            ModelDiscoveryRunner::Codex => Runner::Codex,
            ModelDiscoveryRunner::OpenCode => Runner::OpenCode,
        },
        models,
        source,
        stale,
        reason,
    }
}

async fn read_bounded_stdout(
    child: &mut tokio::process::Child,
) -> Result<(Vec<u8>, std::process::ExitStatus), ()> {
    use tokio::io::AsyncReadExt;
    let mut stdout = child.stdout.take().ok_or(())?;
    tokio::time::timeout(MODEL_DISCOVERY_TIMEOUT, async {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = stdout.read(&mut buffer).await.map_err(|_| ())?;
            if read == 0 {
                break;
            }
            if bytes.len() + read > MAX_MODEL_OUTPUT_BYTES {
                return Err(());
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        let status = child.wait().await.map_err(|_| ())?;
        Ok((bytes, status))
    })
    .await
    .map_err(|_| ())?
}

fn parse_opencode_models(stdout: &str) -> Result<Vec<RunnerModelOption>, ()> {
    let mut models = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut had_line = false;
    for raw_line in stdout.lines() {
        let line = strip_ansi(raw_line).trim().to_owned();
        if line.is_empty() {
            continue;
        }
        had_line = true;
        let Some(slash) = line.find('/') else {
            continue;
        };
        if slash == 0
            || line[slash + 1..].is_empty()
            || !line[..slash]
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            || !line[slash + 1..]
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '/' | '-'))
        {
            continue;
        }
        if !ids.insert(line.clone()) {
            continue;
        }
        if models.len() >= MAX_DISCOVERED_MODELS {
            return Err(());
        }
        let description = format!("via {}", &line[..slash]);
        models.push(RunnerModelOption {
            id: line.clone(),
            label: line,
            description,
            reasoning_efforts: None,
        });
    }
    if had_line && models.is_empty() {
        return Err(());
    }
    Ok(models)
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.next() == Some('[') {
                for character in chars.by_ref() {
                    if character.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                output.push(character);
            }
        } else {
            output.push(character);
        }
    }
    output
}

async fn discover_opencode_models(repo_root: &Path) -> Result<Vec<RunnerModelOption>, ()> {
    let executable = provider_executable(Runner::OpenCode);
    let mut child = tokio::process::Command::new(executable)
        .arg("models")
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let (stdout, status) = read_bounded_stdout(&mut child).await?;
    if !status.success() {
        return Err(());
    }
    parse_opencode_models(&String::from_utf8(stdout).map_err(|_| ())?)
}

async fn write_codex_message(
    stdin: &mut tokio::process::ChildStdin,
    message: Value,
) -> Result<(), ()> {
    use tokio::io::AsyncWriteExt;
    let mut bytes = serde_json::to_vec(&message).map_err(|_| ())?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.map_err(|_| ())
}

async fn read_codex_response(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, ()> {
    while let Some(line) = lines.next_line().await.map_err(|_| ())? {
        let frame: Value = serde_json::from_str(&line).map_err(|_| ())?;
        if frame.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if frame.get("error").is_some() {
            return Err(());
        }
        return Ok(frame
            .get("result")
            .cloned()
            .unwrap_or(Value::Object(Map::new())));
    }
    Err(())
}

async fn discover_codex_models(repo_root: &Path) -> Result<Vec<RunnerModelOption>, ()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let executable = provider_executable(Runner::Codex);
    let mut child = tokio::process::Command::new(executable)
        .arg("app-server")
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let mut stdin = child.stdin.take().ok_or(())?;
    let stdout = child.stdout.take().ok_or(())?;
    let mut lines = BufReader::new(stdout).lines();
    let result = tokio::time::timeout(CODEX_DISCOVERY_TIMEOUT, async {
        write_codex_message(
            &mut stdin,
            json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": { "name": "coducktor", "title": "Coducktor", "version": "0.1.0" },
                    "capabilities": { "experimentalApi": true }
                }
            }),
        )
        .await?;
        read_codex_response(&mut lines, 1).await?;
        write_codex_message(&mut stdin, json!({ "method": "initialized", "params": {} })).await?;
        let mut cursor = Value::Null;
        let mut cursors = std::collections::BTreeSet::new();
        let mut models = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        for page in 0..25_u64 {
            let id = page + 2;
            write_codex_message(
                &mut stdin,
                json!({
                    "id": id,
                    "method": "model/list",
                    "params": { "cursor": cursor, "includeHidden": false }
                }),
            )
            .await?;
            let result = read_codex_response(&mut lines, id).await?;
            let data = result.get("data").and_then(Value::as_array).ok_or(())?;
            for model in data {
                let object = model.as_object().ok_or(())?;
                if object.get("hidden").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let id = object
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or(())?
                    .to_owned();
                if !ids.insert(id.clone()) {
                    continue;
                }
                if models.len() >= MAX_DISCOVERED_MODELS {
                    return Err(());
                }
                let reasoning_efforts = object
                    .get("supportedReasoningEfforts")
                    .map(|value| {
                        value
                            .as_array()
                            .ok_or(())?
                            .iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_owned)
                                    .ok_or(())
                            })
                            .collect::<Result<Vec<_>, ()>>()
                    })
                    .transpose()?;
                models.push(RunnerModelOption {
                    label: object
                        .get("displayName")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(&id)
                        .to_owned(),
                    description: object
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    id,
                    reasoning_efforts,
                });
            }
            let next = result.get("nextCursor").cloned().unwrap_or(Value::Null);
            let Some(next) = next.as_str() else {
                return Ok(models);
            };
            if next.is_empty() || !cursors.insert(next.to_owned()) {
                return Err(());
            }
            cursor = Value::String(next.to_owned());
        }
        Err(())
    })
    .await
    .map_err(|_| ())?;
    let _ = child.kill().await;
    result
}

// ---- open-targets helpers, duplicated from `coducktor-server`'s private functions of the same
// name (renamed `open_targets` -> `open_targets_list` to avoid colliding with the method above)

fn executable_on_path(binary: &str) -> bool {
    if binary.is_empty() {
        return false;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(binary);
        let Ok(metadata) = std::fs::metadata(candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return true;
    }
    false
}

fn configured_executable(provider: &str, default: &str) -> bool {
    let env_name = format!("DUCK_{}_BIN", provider.to_ascii_uppercase());
    std::env::var(&env_name)
        .ok()
        .filter(|path| !path.trim().is_empty())
        .is_some_and(|path| Path::new(&path).is_file())
        || executable_on_path(default)
}

fn installed_mac_app(target: &str) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let names: &[&str] = match target {
        "vscode" => &["Visual Studio Code"],
        "cursor" => &["Cursor"],
        "zed" => &["Zed"],
        "windsurf" => &["Windsurf"],
        "sublime" => &["Sublime Text"],
        "idea" => &[
            "IntelliJ IDEA",
            "IntelliJ IDEA CE",
            "IntelliJ IDEA Ultimate",
        ],
        "pycharm" => &["PyCharm", "PyCharm CE", "PyCharm Professional"],
        "webstorm" => &["WebStorm"],
        "goland" => &["GoLand"],
        "rubymine" => &["RubyMine"],
        "phpstorm" => &["PhpStorm"],
        "clion" => &["CLion"],
        "rider" => &["Rider"],
        "android-studio" => &["Android Studio"],
        "xcode" => &["Xcode"],
        "warp" => &["Warp"],
        _ => return None,
    };
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    names
        .iter()
        .find(|name| {
            roots
                .iter()
                .map(|root| root.join(format!("{name}.app")))
                .any(|path| path.is_dir())
        })
        .copied()
}

fn open_targets_list() -> Vec<coducktor_contract::OpenTarget> {
    let file_manager = if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(target_os = "windows") {
        "Explorer"
    } else {
        "Files"
    };
    let mut targets = vec![
        coducktor_contract::OpenTarget {
            id: "finder".to_owned(),
            label: file_manager.to_owned(),
            icon: Some("folder".to_owned()),
        },
        coducktor_contract::OpenTarget {
            id: "terminal".to_owned(),
            label: "Terminal".to_owned(),
            icon: Some("terminal".to_owned()),
        },
    ];
    for (id, label, icon, binary) in [
        ("vscode", "VS Code", "vscode", "code"),
        ("cursor", "Cursor", "cursor", "cursor"),
        ("zed", "Zed", "zed", "zed"),
        ("windsurf", "Windsurf", "windsurf", "windsurf"),
        ("sublime", "Sublime Text", "sublime", "subl"),
        ("idea", "IntelliJ IDEA", "idea", "idea"),
        ("pycharm", "PyCharm", "pycharm", "pycharm"),
        ("webstorm", "WebStorm", "webstorm", "webstorm"),
        ("goland", "GoLand", "goland", "goland"),
        ("rubymine", "RubyMine", "rubymine", "rubymine"),
        ("phpstorm", "PhpStorm", "phpstorm", "phpstorm"),
        ("clion", "CLion", "clion", "clion"),
        ("rider", "Rider", "rider", "rider"),
        (
            "android-studio",
            "Android Studio",
            "android-studio",
            "studio",
        ),
        ("warp", "Warp", "warp", "warp"),
    ] {
        if executable_on_path(binary) {
            targets.push(coducktor_contract::OpenTarget {
                id: id.to_owned(),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    for (id, label, icon) in [
        ("vscode", "VS Code", "vscode"),
        ("cursor", "Cursor", "cursor"),
        ("zed", "Zed", "zed"),
        ("windsurf", "Windsurf", "windsurf"),
        ("sublime", "Sublime Text", "sublime"),
        ("idea", "IntelliJ IDEA", "idea"),
        ("pycharm", "PyCharm", "pycharm"),
        ("webstorm", "WebStorm", "webstorm"),
        ("goland", "GoLand", "goland"),
        ("rubymine", "RubyMine", "rubymine"),
        ("phpstorm", "PhpStorm", "phpstorm"),
        ("clion", "CLion", "clion"),
        ("rider", "Rider", "rider"),
        ("android-studio", "Android Studio", "android-studio"),
        ("xcode", "Xcode", "xcode"),
        ("warp", "Warp", "warp"),
    ] {
        if installed_mac_app(id).is_some() && !targets.iter().any(|target| target.id == id) {
            targets.push(coducktor_contract::OpenTarget {
                id: id.to_owned(),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    for (provider, label, icon, binary) in [
        ("claude", "Claude CLI", "claude", "claude"),
        ("codex", "Codex CLI", "codex", "codex"),
        ("opencode", "OpenCode", "opencode", "opencode"),
        ("pi", "pi CLI", "pi", "pi"),
    ] {
        if configured_executable(provider, binary) {
            targets.push(coducktor_contract::OpenTarget {
                id: format!("cli:{provider}"),
                label: label.to_owned(),
                icon: Some(icon.to_owned()),
            });
        }
    }
    targets
}

fn open_target_command(target: &str, root: &Path) -> Option<(String, Vec<String>)> {
    if target == "finder" {
        if cfg!(target_os = "macos") {
            return Some(("open".to_owned(), vec![root.to_string_lossy().into_owned()]));
        }
        if cfg!(target_os = "windows") {
            return Some((
                "explorer".to_owned(),
                vec![root.to_string_lossy().into_owned()],
            ));
        }
        return Some((
            "xdg-open".to_owned(),
            vec![root.to_string_lossy().into_owned()],
        ));
    }
    if target == "terminal" {
        if cfg!(target_os = "macos") {
            return Some((
                "open".to_owned(),
                vec![
                    "-a".to_owned(),
                    "Terminal".to_owned(),
                    root.to_string_lossy().into_owned(),
                ],
            ));
        }
        if cfg!(target_os = "windows") {
            return Some((
                "explorer".to_owned(),
                vec![root.to_string_lossy().into_owned()],
            ));
        }
        return Some((
            "x-terminal-emulator".to_owned(),
            vec![
                "--working-directory".to_owned(),
                root.to_string_lossy().into_owned(),
            ],
        ));
    }
    let binary = match target {
        "vscode" => "code",
        "cursor" => "cursor",
        "zed" => "zed",
        "windsurf" => "windsurf",
        "sublime" => "subl",
        "idea" => "idea",
        "pycharm" => "pycharm",
        "webstorm" => "webstorm",
        "goland" => "goland",
        "rubymine" => "rubymine",
        "phpstorm" => "phpstorm",
        "clion" => "clion",
        "rider" => "rider",
        "android-studio" => "studio",
        "xcode" => "xcode",
        "warp" => "warp",
        _ => return None,
    };
    if !executable_on_path(binary)
        && let Some(app) = installed_mac_app(target)
    {
        return Some((
            "open".to_owned(),
            vec![
                "-a".to_owned(),
                app.to_owned(),
                root.to_string_lossy().into_owned(),
            ],
        ));
    }
    executable_on_path(binary)
        .then(|| (binary.to_owned(), vec![root.to_string_lossy().into_owned()]))
}

fn open_target(root: &Path, target: &str) -> bool {
    let Some((program, args)) = open_target_command(target, root) else {
        return false;
    };
    Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

// ---- worktree helpers, duplicated from `coducktor-server`'s private functions of the same name

fn worktree_run_status(status: coducktor_contract::RunStatus) -> WorktreeRunStatus {
    match status {
        coducktor_contract::RunStatus::Queued => WorktreeRunStatus::Queued,
        coducktor_contract::RunStatus::Running => WorktreeRunStatus::Running,
        coducktor_contract::RunStatus::Waiting => WorktreeRunStatus::Waiting,
        coducktor_contract::RunStatus::Review => WorktreeRunStatus::Review,
        coducktor_contract::RunStatus::Done => WorktreeRunStatus::Done,
        coducktor_contract::RunStatus::Failed => WorktreeRunStatus::Failed,
        coducktor_contract::RunStatus::Cancelled => WorktreeRunStatus::Cancelled,
    }
}

fn worktree_size_bytes(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if !metadata.is_dir() {
        return Some(0);
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path).ok()? {
        total = total.checked_add(worktree_size_bytes(&entry.ok()?.path())?)?;
    }
    Some(total)
}

// ---- repo/run git helpers, duplicated from `coducktor-server`'s private functions of the same
// name (git shelling, worktree browsing, diff/compare) ------------------------------------

const NO_WORKTREE: &str = "no worktree — this task ran directly in the repo working tree";
const WORKTREE_FILE_CONTENT_CAP: u64 = 512_000;

fn git_capture(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if error.is_empty() {
            "git command failed".to_owned()
        } else {
            error
        })
    }
}

fn git_capture_owned(root: &Path, args: &[String]) -> Result<String, String> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    git_capture(root, &refs)
}

fn repo_info_at(root: &Path) -> Option<RepoInfo> {
    let repo_root = git_capture(root, &["rev-parse", "--show-toplevel"])
        .ok()?
        .trim()
        .to_owned();
    let branch = git_capture(
        Path::new(&repo_root),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .ok()
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "HEAD".to_owned());
    let remote = git_capture(Path::new(&repo_root), &["remote", "get-url", "origin"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let name = git_capture(Path::new(&repo_root), &["remote"])
                .ok()?
                .lines()
                .map(str::trim)
                .find(|value| !value.is_empty())
                .map(ToOwned::to_owned)?;
            git_capture(Path::new(&repo_root), &["remote", "get-url", name.as_str()])
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        });
    Some(RepoInfo {
        root: repo_root,
        branch,
        remote,
    })
}

fn repo_status(root: &Path) -> Vec<StatusEntry> {
    git_capture(root, &["status", "--porcelain"])
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| StatusEntry {
            status: line.get(..2).unwrap_or(line).trim().to_owned(),
            path: line.get(3..).unwrap_or_default().to_owned(),
        })
        .collect()
}

fn repo_log(root: &Path) -> Vec<LogEntry> {
    git_capture(
        root,
        &["log", "-20", "--pretty=format:%h%x1f%s%x1f%an%x1f%cr"],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
        let mut fields = line.split('\x1f');
        Some(LogEntry {
            hash: fields.next()?.to_owned(),
            subject: fields.next()?.to_owned(),
            author: fields.next()?.to_owned(),
            when: fields.next()?.to_owned(),
        })
    })
    .collect()
}

fn repo_branches(root: &Path) -> Vec<String> {
    let mut branches = std::collections::BTreeSet::new();
    for args in [
        &["branch", "--list", "--format=%(refname:short)"][..],
        &["branch", "-r", "--list", "--format=%(refname:short)"][..],
    ] {
        for name in git_capture(root, args)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
        {
            if name.is_empty() || name.contains("HEAD") {
                continue;
            }
            let name = name.strip_prefix("origin/").unwrap_or(name);
            if !coducktor_core::git::refs::is_task_branch(name) {
                branches.insert(name.to_owned());
            }
        }
    }
    branches.into_iter().collect()
}

fn cap_git_text(text: String, cap: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(cap) else {
        return text;
    };
    format!("{}\n… (diff truncated)", &text[..end])
}

fn diff_revision_args(revisions: &[String], suffix: &[String]) -> Vec<String> {
    let mut args = vec![
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--find-renames".to_owned(),
        "--find-copies".to_owned(),
    ];
    args.extend(revisions.iter().cloned());
    args.extend(suffix.iter().cloned());
    args
}

fn changed_file_status(value: &str) -> ChangedFileStatus {
    match value.chars().next().unwrap_or('M') {
        'A' => ChangedFileStatus::Added,
        'D' => ChangedFileStatus::Deleted,
        'R' => ChangedFileStatus::Renamed,
        'C' => ChangedFileStatus::Copied,
        _ => ChangedFileStatus::Modified,
    }
}

fn collect_git_changes(root: &Path, revisions: &[String]) -> Result<ChangesPayload, String> {
    let names = git_capture_owned(
        root,
        &diff_revision_args(revisions, &["--name-status".to_owned()]),
    )?;
    let numstats = git_capture_owned(
        root,
        &diff_revision_args(revisions, &["--numstat".to_owned()]),
    )?;
    let mut counts = std::collections::HashMap::new();
    for line in numstats.lines() {
        let mut fields = line.split('\t');
        let adds = fields.next().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
        let dels = fields.next().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
        let path = fields.collect::<Vec<_>>().join("\t");
        if !path.is_empty() {
            let path = if let Some((_, new)) = path.rsplit_once(" => ") {
                new.to_owned()
            } else {
                path
            };
            counts.insert(path, (adds, dels, adds.is_nan() || dels.is_nan()));
        }
    }
    let mut files = Vec::new();
    for line in names.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 2 {
            continue;
        }
        let status = changed_file_status(fields[0]);
        let (path, old_path) = if matches!(
            status,
            ChangedFileStatus::Renamed | ChangedFileStatus::Copied
        ) && fields.len() >= 3
        {
            (fields[2].to_owned(), Some(fields[1].to_owned()))
        } else {
            (fields[1].to_owned(), None)
        };
        let (adds, dels, binary) = counts
            .get(&path)
            .copied()
            .map_or((0.0, 0.0, false), |(adds, dels, binary)| {
                (adds, dels, binary)
            });
        let patch_args = diff_revision_args(
            revisions,
            &[
                "--patch".to_owned(),
                "--unified=20".to_owned(),
                "--".to_owned(),
                path.clone(),
            ],
        );
        let patch = git_capture_owned(root, &patch_args).unwrap_or_default();
        let binary = binary || patch.contains("Binary files");
        files.push(ChangedFile {
            path,
            old_path,
            status,
            adds,
            dels,
            binary,
            image: None,
            patch: cap_git_text(patch, 200_000),
        });
    }
    let adds = files.iter().map(|file| file.adds).sum();
    let dels = files.iter().map(|file| file.dels).sum();
    Ok(ChangesPayload {
        stat: RepoDiffStat {
            adds,
            dels,
            files: files.len() as f64,
        },
        files,
        repointed_head: None,
    })
}

fn valid_commit_hash(value: &str) -> bool {
    (4..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn repo_commit_payload(root: &Path, sha: &str) -> Result<RepoCommitPayload, String> {
    if !valid_commit_hash(sha) {
        return Err("not a commit hash".to_owned());
    }
    let metadata = git_capture(
        root,
        &["show", "-s", "--format=%H%x1f%s%x1f%an%x1f%cr", sha],
    )?;
    let mut fields = metadata.trim().split('\x1f');
    let full_sha = fields.next().unwrap_or(sha).to_owned();
    let subject = fields.next().unwrap_or_default().to_owned();
    let author = fields.next().unwrap_or_default().to_owned();
    let when = fields.next().unwrap_or_default().to_owned();
    let parents = git_capture(root, &["rev-list", "--parents", "-n", "1", sha])?;
    let changes = if let Some(parent) = parents.split_whitespace().nth(1) {
        collect_git_changes(root, &[parent.to_owned(), sha.to_owned()])?
    } else {
        ChangesPayload {
            files: Vec::new(),
            stat: RepoDiffStat {
                adds: 0.0,
                dels: 0.0,
                files: 0.0,
            },
            repointed_head: None,
        }
    };
    Ok(RepoCommitPayload {
        sha: full_sha,
        subject,
        author,
        when,
        files: changes.files,
        stat: changes.stat,
    })
}

fn run_changes_payload(root: &Path, base: &str) -> Result<ChangesPayload, String> {
    if !coducktor_core::git::refs::is_safe_git_ref(base) {
        return Err("refusing option-like base ref".to_owned());
    }
    collect_git_changes(root, &[base.to_owned()])
}

fn run_worktree_of(run: &coducktor_contract::RunRecord) -> Option<PathBuf> {
    run.worktree_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn contains_git_component(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| component == std::path::Component::Normal(".git".as_ref()))
}

fn read_worktree_path(root: &Path, relative: &str) -> Result<WorktreeEntry, String> {
    if relative.contains('\0') || contains_git_component(relative) {
        return Err("invalid path".to_owned());
    }
    let real_root =
        std::fs::canonicalize(root).map_err(|_| "worktree is unavailable".to_owned())?;
    let target = root.join(relative);
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| {
        format!(
            "no such file or directory in the worktree: {}",
            if relative.is_empty() { "/" } else { relative }
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err("symlinks are not served".to_owned());
    }
    let real_target =
        std::fs::canonicalize(&target).map_err(|_| "worktree path is unavailable".to_owned())?;
    if real_target != real_root && !real_target.starts_with(&real_root) {
        return Err(format!("path escapes the worktree: {relative}"));
    }
    let display = real_target
        .strip_prefix(&real_root)
        .ok()
        .map(|path| {
            path.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .unwrap_or_default();
    if metadata.is_dir() {
        let mut entries = Vec::new();
        let directory = std::fs::read_dir(&target).map_err(|error| error.to_string())?;
        for entry in directory.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let child_metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) if !metadata.file_type().is_symlink() => metadata,
                _ => continue,
            };
            let entry_type = if child_metadata.is_dir() {
                WorktreeEntryType::Dir
            } else if child_metadata.is_file() {
                WorktreeEntryType::File
            } else {
                continue;
            };
            entries.push(WorktreeDirEntry {
                name,
                entry_type,
                size: child_metadata
                    .is_file()
                    .then_some(child_metadata.len() as f64),
            });
        }
        entries.sort_by(|left, right| {
            let left_dir = matches!(left.entry_type, WorktreeEntryType::Dir);
            let right_dir = matches!(right.entry_type, WorktreeEntryType::Dir);
            right_dir
                .cmp(&left_dir)
                .then_with(|| left.name.cmp(&right.name))
        });
        return Ok(WorktreeEntry::Dir {
            path: display,
            entries,
        });
    }
    if !metadata.is_file() {
        return Err(format!("not a regular file: {display}"));
    }
    let size = metadata.len();
    let too_large = size > WORKTREE_FILE_CONTENT_CAP;
    let mut sample = Vec::new();
    if let Ok(mut file) = std::fs::File::open(&target) {
        use std::io::Read as _;
        let mut buffer = [0_u8; 8_192];
        let read = file.read(&mut buffer).unwrap_or(0);
        sample.extend_from_slice(&buffer[..read]);
    }
    let binary = sample.contains(&0);
    let content = if binary || too_large {
        None
    } else {
        std::fs::read(&target)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    Ok(WorktreeEntry::File {
        path: display,
        size: size as f64,
        binary,
        too_large,
        content,
    })
}

fn image_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Ported from `run_files`'s `wants_raw` branch: raw bytes are only ever served for an image
/// that isn't over the content cap — everything else is a `Conflict`, matching the oracle's own
/// `raw serving is limited to images` / `file too large to serve raw` messages.
fn read_worktree_raw(root: &Path, relative: &str) -> Result<Vec<u8>, EngineError> {
    let entry =
        read_worktree_path(root, relative).map_err(|reason| EngineError::Conflict { reason })?;
    let WorktreeEntry::File {
        path, too_large, ..
    } = &entry
    else {
        return Err(EngineError::Conflict {
            reason: format!("raw serving is limited to images: {relative}"),
        });
    };
    let mime = image_content_type(Path::new(path));
    if !mime.starts_with("image/") {
        return Err(EngineError::Conflict {
            reason: format!("raw serving is limited to images: {path}"),
        });
    }
    if *too_large {
        return Err(EngineError::Conflict {
            reason: format!("file too large to serve raw: {path}"),
        });
    }
    std::fs::read(root.join(path)).map_err(io_err)
}

fn repo_response(repo_root: &Path) -> RepoResponse {
    let Some(info) = repo_info_at(repo_root) else {
        return RepoResponse::Empty(EmptyRepoResponse {
            info: None,
            status: Vec::new(),
            log: Vec::new(),
            branches: Vec::new(),
            base_branch: None,
        });
    };
    let workspace = workspace_config_for(repo_root);
    let config =
        coducktor_core::config::load_config(Path::new(&info.root), &workspace.agent_defaults);
    RepoResponse::Present(PresentRepoResponse {
        info: info.clone(),
        status: repo_status(Path::new(&info.root)),
        log: repo_log(Path::new(&info.root)),
        branches: repo_branches(Path::new(&info.root)),
        base_branch: config.base_branch,
    })
}

fn create_repo_branch(
    repo_root: &Path,
    input: &RepoBranchRequest,
) -> Result<RepoBranchResponse, EngineError> {
    let Some(info) = repo_info_at(repo_root) else {
        return Err(EngineError::Conflict {
            reason: "not a git repository".to_owned(),
        });
    };
    let name = input.name.trim();
    if name.is_empty() || name.len() > 200 || !coducktor_core::git::refs::is_safe_git_ref(name) {
        return Err(EngineError::Conflict {
            reason: format!("invalid branch name: {name}"),
        });
    }
    let root = Path::new(&info.root);
    if git_capture(root, &["check-ref-format", "--branch", name]).is_err() {
        return Err(EngineError::Conflict {
            reason: format!("invalid branch name: {name}"),
        });
    }
    let exists = git_capture(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok();
    let args = if exists {
        vec!["checkout".to_owned(), name.to_owned()]
    } else if let Some(from) = input
        .from
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !coducktor_core::git::refs::is_safe_git_ref(from)
            || git_capture(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{from}^{{commit}}"),
                ],
            )
            .is_err()
        {
            return Err(EngineError::Conflict {
                reason: format!("unknown start point: {from}"),
            });
        }
        vec![
            "checkout".to_owned(),
            "-b".to_owned(),
            name.to_owned(),
            from.to_owned(),
        ]
    } else {
        vec!["checkout".to_owned(), "-b".to_owned(), name.to_owned()]
    };
    if let Err(error) = git_capture_owned(root, &args) {
        return Err(EngineError::Conflict { reason: error });
    }
    Ok(RepoBranchResponse {
        branch: name.to_owned(),
        created: !exists,
    })
}

// ---- agent-config helpers, duplicated from `coducktor-server`'s private AGENT_CONFIG_DEFINITIONS
// catalog and its supporting functions --------------------------------------------------------

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
    let homes = agent_home_paths(&ProcessEnv);
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
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agent_config_content(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
) -> Result<AgentConfigFileContent, String> {
    let path = resolve_agent_config_path(definition, repo_root);
    match std::fs::read(&path) {
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
    let homes = agent_home_paths(&ProcessEnv);
    let default_home = real_home_dir(&ProcessEnv).join(".claude");
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
    let Ok(metadata) = std::fs::metadata(&path) else {
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
    let Ok(raw) = std::fs::read_to_string(&path) else {
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
            let metadata = std::fs::metadata(&path).ok();
            let (exists, size, version) = match metadata {
                Some(metadata) => {
                    let version = std::fs::read(&path).ok().map(|bytes| config_hash(&bytes));
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

fn write_agent_config(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
    input: SetAgentConfigInput,
) -> Result<AgentConfigFileContent, EngineError> {
    if let Err(error) = validate_agent_config(&input.content, definition.format) {
        return Err(EngineError::Conflict {
            reason: format!("Invalid {:?}: {error}", definition.format).to_lowercase(),
        });
    }
    let path = resolve_agent_config_path(definition, repo_root);
    let current = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_err(error)),
    };
    if input.content.trim().is_empty()
        && current
            .as_ref()
            .is_some_and(|bytes| !String::from_utf8_lossy(bytes).trim().is_empty())
    {
        return Err(EngineError::Conflict {
            reason: "refusing to overwrite a non-empty config file with empty content — delete the file manually if you mean to remove it"
                .to_owned(),
        });
    }
    let current_version = current.as_deref().map(config_hash);
    if current_version != input.version {
        return Err(EngineError::Conflict {
            reason: if current_version.is_none() {
                "the file no longer exists on disk — reload before saving".to_owned()
            } else {
                "the file changed on disk since you opened it — reload before saving".to_owned()
            },
        });
    }
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if let Some(parent) = target.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Err(io_err(error));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = PathBuf::from(format!(
        "{}.duck-tmp-{}-{nonce}",
        target.display(),
        std::process::id()
    ));
    if let Err(error) = std::fs::write(&temporary, input.content.as_bytes()) {
        return Err(io_err(error));
    }
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_err(error));
    }
    agent_config_content(definition, repo_root).map_err(EngineError::Transport)
}

// ---- IDE helpers, duplicated from `coducktor-server`'s private ide_* functions -----------

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

fn normalize_ide_path(root: &Path, path: &str) -> Result<PathBuf, EngineError> {
    if path.chars().count() > 4_096
        || path.contains('\0')
        || path.contains('\\')
        || Path::new(path).is_absolute()
    {
        return Err(EngineError::Conflict {
            reason: "invalid project path".to_owned(),
        });
    }
    let mut target = root.to_path_buf();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(value) => target.push(value),
            std::path::Component::ParentDir => {
                if target == root || !target.pop() {
                    return Err(EngineError::Conflict {
                        reason: "path is outside the project".to_owned(),
                    });
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(EngineError::Conflict {
                    reason: "invalid project path".to_owned(),
                });
            }
        }
    }
    Ok(target)
}

fn resolve_ide_path(
    root: &Path,
    path: &str,
    directory: bool,
) -> Result<(PathBuf, PathBuf), EngineError> {
    let project_root = std::fs::canonicalize(root).map_err(|_| EngineError::NotFound)?;
    let lexical = normalize_ide_path(&project_root, path)?;
    let target = std::fs::canonicalize(&lexical).map_err(|_| EngineError::NotFound)?;
    if !target.starts_with(&project_root) {
        return Err(EngineError::Conflict {
            reason: "path is outside the project".to_owned(),
        });
    }
    if target != lexical {
        return Err(EngineError::Conflict {
            reason: "symbolic links are not editable".to_owned(),
        });
    }
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| EngineError::NotFound)?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(EngineError::NotFound);
    }
    Ok((project_root, target))
}

fn ide_list_directory(root: &Path, path: &str) -> Result<IdeDirectoryResponse, EngineError> {
    let (project_root, target) = resolve_ide_path(root, path, true)?;
    let entries = std::fs::read_dir(&target).map_err(|_| EngineError::NotFound)?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| EngineError::NotFound)?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| EngineError::NotFound)?;
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
        } else if let Ok(metadata) = std::fs::metadata(&entry_path)
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

fn ide_read_file(root: &Path, path: &str) -> Result<IdeFileResponse, EngineError> {
    if path.is_empty() {
        return Err(EngineError::Conflict {
            reason: "path is required".to_owned(),
        });
    }
    let (project_root, target) = resolve_ide_path(root, path, false)?;
    let metadata = std::fs::metadata(&target).map_err(|_| EngineError::NotFound)?;
    if metadata.len() > IDE_FILE_MAX_BYTES as u64 {
        return Err(EngineError::Conflict {
            reason: "file is too large to edit".to_owned(),
        });
    }
    let bytes = std::fs::read(&target).map_err(|_| EngineError::NotFound)?;
    if bytes.contains(&0) {
        return Err(EngineError::Conflict {
            reason: "binary files cannot be edited".to_owned(),
        });
    }
    let content = String::from_utf8(bytes.clone()).map_err(|_| EngineError::Conflict {
        reason: "binary files cannot be edited".to_owned(),
    })?;
    Ok(IdeFileResponse {
        path: ide_display_path(&project_root, &target),
        content,
        size: bytes.len() as u64,
    })
}

fn ide_write_file(root: &Path, path: &str, content: &str) -> Result<IdeFileResponse, EngineError> {
    if path.is_empty() {
        return Err(EngineError::Conflict {
            reason: "path is required".to_owned(),
        });
    }
    if content.len() > IDE_FILE_MAX_BYTES {
        return Err(EngineError::Conflict {
            reason: "file is too large to edit".to_owned(),
        });
    }
    let (_, target) = resolve_ide_path(root, path, false)?;
    std::fs::write(&target, content.as_bytes()).map_err(|error| EngineError::Conflict {
        reason: error.to_string(),
    })?;
    ide_read_file(root, path)
}

// ---- per-repo config helpers, duplicated from `coducktor-server`'s private config_response/
// parse_set_config_input/config_models_locked/read_repo_config functions -------------------

fn repo_config_path(repo_root: &Path) -> PathBuf {
    data_dir(repo_root).join("config.json")
}

fn read_repo_config(repo_root: &Path) -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(repo_config_path(repo_root)) else {
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

fn config_response(repo_root: &Path) -> ConfigResponse {
    let workspace = workspace_config_for(repo_root);
    let config = coducktor_core::config::load_config(repo_root, &workspace.agent_defaults);
    let models_locked = config_models_locked(repo_root, &config);
    ConfigResponse {
        base_branch: config.base_branch,
        default_runner: config.default_runner,
        system_prompt: config.system_prompt,
        default_models: if models_locked {
            coducktor_contract::RunnerModels::default()
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
    Ok(())
}

fn update_repo_config(
    repo_root: &Path,
    input: &SetConfigInput,
) -> Result<ConfigResponse, EngineError> {
    validate_set_config_input(input)?;
    let workspace = workspace_config_for(repo_root);
    let current = coducktor_core::config::load_config(repo_root, &workspace.agent_defaults);
    if config_models_locked(repo_root, &current) && input.default_models.is_some() {
        return Err(EngineError::Conflict {
            reason:
                "agent models are locked — configure the model in the native coding-agent settings"
                    .to_owned(),
        });
    }

    let mut raw = read_repo_config(repo_root);
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
    coducktor_core::workspace::config::atomic_write_json_sync(
        &repo_config_path(repo_root),
        &Value::Object(raw),
    )
    .map_err(io_err)?;
    Ok(config_response(repo_root))
}

// ---- workflow builder helpers, duplicated from `coducktor-server`'s private functions of the
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

// ---- agent-profile + provider-status helpers, duplicated from `coducktor-server`'s private
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

/// Ported from `coducktor-server`'s private `allocate_project_id` — account ids share the same
/// slug-collision-avoidance scheme (and, quirk inherited from the oracle rather than introduced
/// here, the same "project" fallback word for an unslugifiable label) project ids use.
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
            let mut counts = lower
                .lines()
                .filter_map(|line| {
                    let mut words = line.split_whitespace();
                    let count = words.next()?.parse::<u64>().ok()?;
                    words.next().filter(|word| word.starts_with("credential"))?;
                    Some(count)
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

/// Best-effort "open with the OS default app" — mirrors `coducktor-server`'s
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
    RunIndexEntry {
        project_id: project_id.to_owned(),
        id: run.id,
        title: run.title,
        title_summary: run.title_summary,
        title_origin: run.title_origin,
        status: run.status,
        activity: run.activity,
        created_at: run.created_at,
        finished_at: run.finished_at,
        seen_at: run.seen_at,
        archived: run.archived,
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
        model_usage: None,
        model_identity: run.model_identity,
        reasoning_effort: None,
    }
}

fn todo_workflow(repo_root: &Path, todo: &TodoItem) -> coducktor_contract::WorkflowDef {
    if let Some(skill_name) = todo.suggested_skill.as_deref() {
        let skills = discover_skills(repo_root, &ProcessEnv);
        if skills.iter().any(|skill| skill.name == skill_name) {
            return coducktor_contract::WorkflowDef {
                name: "(inbox)".to_owned(),
                description: Some(format!("Follow-up from the inbox — skill \"{skill_name}\"")),
                steps: vec![coducktor_contract::WorkflowStepDef {
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

/// Ported from `coducktor-server::project_entry` — resolves each project's live git status/
/// branch on every call, same as the oracle (no caching either side of this port does).
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

fn repo_ui_state_path(repo_root: &Path) -> PathBuf {
    data_dir(repo_root).join("ui-state.json")
}

fn read_repo_ui_state(repo_root: &Path) -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(repo_ui_state_path(repo_root)) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn health_payload(repo_root: &Path, version: &str, probe_backends: bool) -> HealthResponse {
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let branch = git_output(repo_root, &["branch", "--show-current"]);
    let remote = git_output(repo_root, &["config", "--get", "remote.origin.url"]);
    let forge_available = resolve_forge(repo_root.to_path_buf(), remote.as_deref()).is_some();
    let repo = if branch.is_some() || remote.is_some() {
        Some(RepoInfo {
            root: repo_root_str.clone(),
            branch: branch.unwrap_or_default(),
            remote,
        })
    } else {
        None
    };
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
        capabilities: Capabilities {
            followups: followups_enabled(&ProcessEnv),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::WorkflowStepDef;
    use coducktor_core::workflows::run::{
        AgentSession, EventInput, SessionFactory, SessionRequest,
    };
    use std::io;
    use tempfile::TempDir;

    /// A session that immediately completes with `DUCK:DONE` — enough to prove
    /// `InProcessEngine`'s own wiring (queueing, persistence, event fan-out) without spawning a
    /// real agent CLI; the four real backends already have their own dedicated subprocess
    /// tests in `coducktor-runners`.
    struct FakeSession;
    impl AgentSession for FakeSession {
        fn turn(
            &mut self,
            on_event: &mut dyn FnMut(EventInput) -> io::Result<()>,
        ) -> Result<coducktor_core::workflows::run::SessionOutcome, String> {
            on_event(EventInput::new("text").field("text", "done (fake)"))
                .map_err(|error| error.to_string())?;
            Ok(coducktor_core::workflows::run::SessionOutcome::Completed(
                coducktor_core::workflows::run::SessionReport {
                    session_id: Some("fake-session".to_owned()),
                    tokens_used: 10.0,
                    input_tokens: Some(5.0),
                    output_tokens: Some(5.0),
                    cost_usd: None,
                    turn_text: "done (fake)\n\nDUCK:DONE".to_owned(),
                    decision: Some(coducktor_core::workflows::run::TurnMarkerDecision::Done),
                    plan_entries: None,
                },
            ))
        }

        fn session_id(&self) -> Option<String> {
            Some("fake-session".to_owned())
        }
    }

    struct FakeFactory;
    impl SessionFactory for FakeFactory {
        fn open(
            &mut self,
            _request: SessionRequest,
        ) -> Result<Box<dyn AgentSession + Send>, String> {
            Ok(Box::new(FakeSession))
        }
    }

    fn engine(dir: &TempDir) -> InProcessEngine {
        InProcessEngine::with_session_factory(dir.path(), "0.0.0-test", FakeFactory)
    }

    fn steps_input(task: &str) -> CreateRunInput {
        CreateRunInput {
            workflow: None,
            steps: Some(vec![WorkflowStepDef {
                id: "task".to_owned(),
                name: Some("Task".to_owned()),
                prompt: Some("{{task}}".to_owned()),
                skill: None,
                model: None,
                runner: None,
                allowed_tools: None,
                bash_allowlist: None,
                command: None,
                on_fail: None,
            }]),
            task: task.to_owned(),
            model: None,
            reasoning_effort: None,
            runner: None,
            agent_profile: None,
            variants: None,
            worktree: Some(false),
            autonomous: None,
            generate_followups: None,
            system_prompt: None,
            images: None,
            todo_id: None,
        }
    }

    #[tokio::test]
    async fn health_reports_the_configured_version_and_repo_root() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let health = engine.health().await.unwrap();
        assert_eq!(health.version, "0.0.0-test");
        assert_eq!(health.repo_root, dir.path().to_string_lossy());
        assert!(!health.checks.is_empty());
    }

    #[tokio::test]
    async fn a_run_started_with_inline_steps_completes_via_the_fake_session() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        assert_eq!(run.status, coducktor_contract::RunStatus::Done);

        let listed = engine.list_runs().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].record.id, run.id);

        let fetched = engine.get_run(&run.id).await.unwrap();
        assert_eq!(fetched.record.id, run.id);
    }

    #[tokio::test]
    async fn start_run_rejects_empty_steps() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let mut input = steps_input("x");
        input.steps = Some(vec![]);
        let error = engine.start_run(input).await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn get_run_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        assert_eq!(engine.get_run("nope").await, Err(EngineError::NotFound));
    }

    #[tokio::test]
    async fn archive_delete_read_unread_round_trip() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("archive me")).await.unwrap()
        else {
            panic!("expected a single run");
        };

        let archived = engine.archive_run(&run.id, true).await.unwrap();
        assert!(archived.record.archived);
        let unarchived = engine.archive_run(&run.id, false).await.unwrap();
        assert!(!unarchived.record.archived);

        let read = engine.read_run(&run.id).await.unwrap();
        assert!(read.record.seen_at.is_some());
        let unread = engine.unread_run(&run.id).await.unwrap();
        assert!(unread.record.seen_at.is_none());

        let deleted = engine.delete_run(&run.id).await.unwrap();
        assert!(deleted.deleted);
        assert_eq!(engine.get_run(&run.id).await, Err(EngineError::NotFound));
    }

    #[tokio::test]
    async fn archive_finished_and_mark_all_read_report_counts() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let CreateRunResponse::Single(one) = engine.start_run(steps_input("one")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        let CreateRunResponse::Single(two) = engine.start_run(steps_input("two")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        // A run completed via the fake session is already "seen" by the time `start_run`
        // returns (there was no live cockpit watching it) — mark both explicitly unread first
        // so `mark_all_read` has something real to count.
        engine.unread_run(&one.id).await.unwrap();
        engine.unread_run(&two.id).await.unwrap();

        // `mark_all_read` before `archive_finished`: `is_unread` excludes archived runs, so
        // order matters here the same way it would through any other caller.
        let read = engine.mark_all_read().await.unwrap();
        assert_eq!(read.read, 2.0);
        let archived = engine.archive_finished().await.unwrap();
        assert_eq!(archived.archived, 2.0);
    }

    #[tokio::test]
    async fn patch_run_renames_a_queued_run_but_not_a_started_one() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("rename me")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        // The fake session completes synchronously, so by the time `start_run` returns the run
        // is already `done`, not `queued` — renaming its title (not its task) is still allowed.
        let patched = engine
            .patch_run(
                &run.id,
                PatchRunInput {
                    title: Some("new title".to_owned()),
                    task: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(patched.record.title, "new title");

        let error = engine
            .patch_run(
                &run.id,
                PatchRunInput {
                    title: None,
                    task: Some("swap the task".to_owned()),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn cancel_and_finish_report_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        assert_eq!(engine.cancel_run("nope").await, Err(EngineError::NotFound));
        assert_eq!(engine.finish_run("nope").await, Err(EngineError::NotFound));
    }

    #[tokio::test]
    async fn workflows_and_skills_read_from_the_repo_root() {
        let dir = TempDir::new().unwrap();
        let workflows_dir = dir.path().join(".ai/coducktor/workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("demo.yaml"),
            "name: demo\nsteps:\n  - id: task\n    prompt: \"{{task}}\"\n",
        )
        .unwrap();
        let engine = engine(&dir);
        let workflows = engine.workflows().await.unwrap();
        assert!(workflows.workflows.iter().any(|w| w.name == "demo"));

        // `discover_skills` also reads global, host-level locations, so this asserts only that
        // the call succeeds rather than that the (sandbox-dependent) count is zero.
        engine.skills().await.unwrap();
    }

    #[tokio::test]
    async fn ui_state_round_trips_a_shallow_merge() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        assert_eq!(engine.ui_state().await.unwrap(), json!({}));

        let merged = engine
            .put_ui_state(json!({ "sidebarWidth": 240 }))
            .await
            .unwrap();
        assert_eq!(merged, json!({ "sidebarWidth": 240 }));

        let merged_again = engine
            .put_ui_state(json!({ "theme": "dark" }))
            .await
            .unwrap();
        assert_eq!(
            merged_again,
            json!({ "sidebarWidth": 240, "theme": "dark" })
        );

        assert_eq!(engine.ui_state().await.unwrap(), merged_again);
    }

    #[tokio::test]
    async fn todos_are_empty_when_followups_are_disabled() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // DUCK_FOLLOWUPS is unset in the test process by default.
        assert!(engine.todos().await.unwrap().is_empty());
        assert_eq!(
            engine.delete_todo("anything").await,
            Err(EngineError::Conflict {
                reason: "the follow-up inbox is disabled — set DUCK_FOLLOWUPS=1 to enable it"
                    .to_owned()
            })
        );
    }

    #[tokio::test]
    async fn projects_reports_the_registry_snapshot() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // No `~/.coducktor/config.json` is guaranteed to exist in a test sandbox; either way
        // the call must succeed with an empty (or real) registry, never error.
        let projects = engine.projects().await.unwrap();
        assert!(projects.projects.iter().all(|p| !p.id.is_empty()));
    }

    #[tokio::test]
    async fn subscribe_receives_a_run_event_published_during_start_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // Proves the subscribe -> broadcast -> topic-filtered-stream path end-to-end. A real
        // run's own events are covered indirectly by every other test in this module (each
        // starts a run through the same `manager.subscribe_events`/`subscribe_runs` wiring
        // this constructs); publishing directly here isolates the transport itself.
        let mut stream = engine.subscribe(Topic::Health);
        engine
            .live_events
            .send(EngineEvent {
                topic: "health".to_owned(),
                data: json!({ "ok": true }),
            })
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("event should arrive")
            .expect("stream should not end");
        assert_eq!(event.data, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn implements_the_full_engine_trait_without_http() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let engine: &dyn crate::Engine = &engine;
        let scope = crate::Scope::Workspace;

        assert!(engine.list_runs(&scope).await.unwrap().is_empty());
        assert_eq!(
            engine.ui_state(&scope).await.unwrap(),
            coducktor_contract::UiState::default()
        );
    }

    #[tokio::test]
    async fn workspace_usage_reports_no_providers_matching_the_b10_scope_cut() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        assert_eq!(engine.workspace_usage().await.unwrap().providers, vec![]);
    }

    fn save_input(name: &str, steps: Vec<WorkflowStepDef>) -> SaveWorkflowInput {
        SaveWorkflowInput {
            name: name.to_owned(),
            description: None,
            steps: Some(steps),
            skills: None,
            overwrite: None,
        }
    }

    fn one_step() -> Vec<WorkflowStepDef> {
        vec![WorkflowStepDef {
            id: "task".to_owned(),
            name: Some("Task".to_owned()),
            prompt: Some("{{task}}".to_owned()),
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        }]
    }

    #[tokio::test]
    async fn save_workflow_writes_a_yaml_file_the_loader_can_read_back() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine
            .save_workflow(&save_input("My Workflow", one_step()))
            .await
            .unwrap();
        assert!(response.path.ends_with("my-workflow.yaml"));
        assert_eq!(response.name, "My Workflow");

        let (workflows, issues) = load_workflows(dir.path());
        assert!(issues.is_empty());
        assert!(workflows.iter().any(|w| w.name == "My Workflow"));
    }

    #[tokio::test]
    async fn save_workflow_conflicts_on_an_existing_file_without_overwrite() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine
            .save_workflow(&save_input("Dup", one_step()))
            .await
            .unwrap();
        let error = engine
            .save_workflow(&save_input("Dup", one_step()))
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn save_workflow_rejects_steps_and_skills_together() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let mut input = save_input("Both", one_step());
        input.skills = Some(vec!["a-skill".to_owned()]);
        let error = engine.save_workflow(&input).await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn delete_workflow_removes_a_file_the_builder_saved() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine
            .save_workflow(&save_input("To Delete", one_step()))
            .await
            .unwrap();
        let response = engine.delete_workflow("To Delete").await.unwrap();
        assert!(response.ok);
        let (workflows, _) = load_workflows(dir.path());
        assert!(!workflows.iter().any(|w| w.name == "To Delete"));
    }

    #[tokio::test]
    async fn delete_workflow_refuses_a_built_in_workflow() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // `quick-task` is the built-in with no on-disk `path` — see `workflows::types`.
        let error = engine.delete_workflow("quick-task").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn delete_workflow_reports_not_found_for_an_unknown_name() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.delete_workflow("nope").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn parse_workflow_normalizes_valid_yaml() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let yaml = "name: Parsed\nsteps:\n  - id: task\n    prompt: \"{{task}}\"\n";
        let parsed = engine.parse_workflow(yaml).await.unwrap();
        assert_eq!(parsed.name, "Parsed");
        assert_eq!(parsed.steps.len(), 1);
        assert_eq!(parsed.steps[0].id, "task");
    }

    #[tokio::test]
    async fn parse_workflow_rejects_malformed_yaml() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.parse_workflow("not: [valid").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn parse_workflow_rejects_an_empty_document() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.parse_workflow("   ").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    // ---- provider status + agent-profile accounts ------------------------------------------
    //
    // `create_agent_profile`/`update_agent_profile`/`remove_agent_profile`/`select_agent_profile`
    // (like the `coducktor-server` handlers they're ported from) resolve their storage path via
    // `agent_accounts_path(&ProcessEnv)` — the REAL `~/.coducktor/agent-accounts.json` (or
    // `$DUCK_HOME` if set), with no injectable override. No test here calls one of
    // these methods down a path that would actually write to it: every write-path test below
    // exercises validation that returns before any file I/O happens (matching the same
    // established "safe against a real, possibly-populated environment" discipline the existing
    // `projects_reports_the_registry_snapshot` test above already relies on for its own
    // read-only call). A full create/update/remove round-trip against an isolated
    // `agent-accounts.json` is not covered here — it would need `agent_accounts_path`/
    // `workspace_config_path` to accept an injected `EnvSource` the way `coducktor-core`'s lower-
    // level `load_agent_accounts`/`merge_write_agent_accounts` already do, which is a real gap in
    // the *oracle* this ports from, not something introduced here.

    #[tokio::test]
    async fn provider_status_reports_one_entry_per_provider() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let status = engine.provider_status().await.unwrap();
        assert_eq!(status.providers.len(), PROVIDER_IDS.len());
        for provider in PROVIDER_IDS {
            assert!(status.providers.iter().any(|p| p.provider == provider));
        }
    }

    #[tokio::test]
    async fn agent_profiles_always_includes_the_four_default_profiles() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.agent_profiles().await.unwrap();
        for provider in PROVIDER_IDS {
            assert!(
                response
                    .profiles
                    .iter()
                    .any(|profile| profile.provider == provider && profile.is_default)
            );
        }
        assert_eq!(
            response.profile_capable_providers,
            vec![Runner::Claude, Runner::Codex]
        );
    }

    #[tokio::test]
    async fn create_agent_profile_rejects_a_provider_that_cannot_carry_extra_accounts() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .create_agent_profile(&CreateAgentProfileInput {
                provider: Runner::OpenCode,
                label: None,
                config_dir: "/tmp/wherever".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn create_agent_profile_rejects_a_relative_config_dir() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .create_agent_profile(&CreateAgentProfileInput {
                provider: Runner::Claude,
                label: None,
                config_dir: "relative/path".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn update_agent_profile_requires_at_least_one_field() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .update_agent_profile(
                "whatever",
                &UpdateAgentProfileInput {
                    label: None,
                    config_dir: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn update_agent_profile_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .update_agent_profile(
                "coducktor-test-account-that-does-not-exist",
                &UpdateAgentProfileInput {
                    label: Some("New label".to_owned()),
                    config_dir: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn remove_agent_profile_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .remove_agent_profile("coducktor-test-account-that-does-not-exist")
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn select_agent_profile_reports_not_found_for_an_unknown_project() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .select_agent_profile(&SelectAgentProfileInput {
                project_id: Some("coducktor-test-project-that-does-not-exist".to_owned()),
                provider: Runner::Claude,
                profile_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn agent_account_status_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .agent_account_status("coducktor-test-account-that-does-not-exist", false)
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn agent_account_status_resolves_a_default_profile_by_its_synthetic_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // `default:<provider>` never 404s — it always resolves to the built-in profile, even
        // with nothing configured.
        let status = engine
            .agent_account_status("default:claude", true)
            .await
            .unwrap();
        assert_eq!(status.status.provider, Runner::Claude);
    }

    #[tokio::test]
    async fn agent_account_details_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .agent_account_details("coducktor-test-account-that-does-not-exist")
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn open_agent_account_file_rejects_a_cli_target_before_touching_disk() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // No account with this id exists either, but an explicit `target` must be rejected
        // first — proves the check happens before any lookup, not just before any I/O.
        let error = engine
            .open_agent_account_file(
                "coducktor-test-account-that-does-not-exist",
                &OpenAgentAccountFileInput {
                    file: "folder".to_owned(),
                    target: Some("cli:codex".to_owned()),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn open_agent_account_file_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .open_agent_account_file(
                "coducktor-test-account-that-does-not-exist",
                &OpenAgentAccountFileInput {
                    file: "folder".to_owned(),
                    target: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    // ---- pure helper functions (no `ProcessEnv`/filesystem-default resolution involved) -----

    #[test]
    fn account_slug_lowercases_and_dashes_non_alphanumerics() {
        assert_eq!(account_slug("My Claude Account!"), "my-claude-account-");
    }

    #[test]
    fn allocate_account_id_dedupes_against_taken_ids_and_reserved_words() {
        let mut taken = std::collections::BTreeSet::new();
        taken.insert("work".to_owned());
        assert_eq!(allocate_account_id("Work", &taken), "work-2");
        taken.clear();
        taken.insert("default".to_owned()); // not actually taken, but a reserved word
        assert_eq!(allocate_account_id("default", &taken), "default-2");
    }

    #[test]
    fn allocate_account_id_falls_back_to_project_for_an_unslugifiable_label() {
        // Matches the oracle's own quirk (see `allocate_account_id`'s doc comment) verbatim.
        assert_eq!(
            allocate_account_id("!!!", &std::collections::BTreeSet::new()),
            "project"
        );
    }

    #[test]
    fn provider_state_from_output_reads_claudes_logged_in_json() {
        assert_eq!(
            provider_state_from_output(Runner::Claude, r#"{"loggedIn":true}"#, "", Some(0)),
            Some(ProviderConnectionState::Connected)
        );
        assert_eq!(
            provider_state_from_output(Runner::Claude, r#"{"loggedIn":false}"#, "", Some(1)),
            Some(ProviderConnectionState::Disconnected)
        );
        assert_eq!(
            provider_state_from_output(Runner::Claude, "not json", "", Some(0)),
            None
        );
    }

    #[test]
    fn provider_state_from_output_reads_codexs_status_lines() {
        assert_eq!(
            provider_state_from_output(Runner::Codex, "Logged in using ChatGPT\n", "", Some(0)),
            Some(ProviderConnectionState::Connected)
        );
        assert_eq!(
            provider_state_from_output(Runner::Codex, "Not logged in\n", "", Some(1)),
            Some(ProviderConnectionState::Disconnected)
        );
    }

    #[test]
    fn identity_text_prefers_a_non_empty_string_then_falls_back_to_a_number() {
        assert_eq!(
            identity_text(Some(&json!("  Jane  "))),
            Some("Jane".to_owned())
        );
        assert_eq!(identity_text(Some(&json!(""))), None);
        assert_eq!(identity_text(Some(&json!(42))), Some("42".to_owned()));
        assert_eq!(identity_text(Some(&json!(null))), None);
        assert_eq!(identity_text(None), None);
    }

    #[test]
    fn same_profile_dir_matches_identical_paths_without_touching_disk() {
        assert!(same_profile_dir(Path::new("/a/b/c"), Path::new("/a/b/c")));
    }

    #[test]
    fn same_profile_dir_resolves_distinct_existing_paths_via_canonicalization() {
        let dir = TempDir::new().unwrap();
        let target = dir.path();
        let via_dot = target.join(".");
        assert!(same_profile_dir(target, &via_dot));
    }

    #[test]
    fn same_profile_dir_does_not_match_two_missing_distinct_paths() {
        assert!(!same_profile_dir(
            Path::new("/coducktor-test/does-not-exist-a"),
            Path::new("/coducktor-test/does-not-exist-b")
        ));
    }

    #[test]
    fn profile_dir_state_reports_a_marker_file_as_looking_valid() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("settings.json"), "{}").unwrap();
        let profile = ResolvedAgentProfile {
            id: coducktor_contract::DEFAULT_AGENT_ACCOUNT_ID.to_owned(),
            provider: Runner::Claude,
            label: "Default".to_owned(),
            config_dir: dir.path().to_string_lossy().into_owned(),
            path: dir.path().to_path_buf(),
            is_default: true,
        };
        let (exists, looks_valid) = profile_dir_state(&profile);
        assert!(exists);
        assert!(looks_valid);
    }

    #[test]
    fn profile_dir_state_reports_a_missing_directory_as_not_existing() {
        let profile = ResolvedAgentProfile {
            id: "acct".to_owned(),
            provider: Runner::Codex,
            label: "Acct".to_owned(),
            config_dir: "/coducktor-test/does-not-exist".to_owned(),
            path: PathBuf::from("/coducktor-test/does-not-exist"),
            is_default: false,
        };
        let (exists, looks_valid) = profile_dir_state(&profile);
        assert!(!exists);
        assert!(!looks_valid);
    }

    #[test]
    fn agent_profile_wire_reports_zero_files_for_pi_which_has_no_config_files() {
        let profile = ResolvedAgentProfile {
            id: coducktor_contract::DEFAULT_AGENT_ACCOUNT_ID.to_owned(),
            provider: Runner::Pi,
            label: "Default".to_owned(),
            config_dir: String::new(),
            path: PathBuf::new(),
            is_default: true,
        };
        let wire = agent_profile_wire(&profile);
        assert!(wire.files.is_empty());
        assert!(wire.is_default);
    }

    // ---- IDE (C1 continuation) -----------------------------------------------------------

    #[tokio::test]
    async fn ide_tree_lists_directories_before_files_alphabetically() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir); // creates `.ai/coducktor/**` as a side effect — expected in the listing, `.git` is the only exclusion the oracle makes
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), b"hi").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let tree = engine.ide_tree(None).await.unwrap();
        let names: Vec<&str> = tree
            .entries
            .iter()
            .filter(|e| e.name != ".ai")
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "README.md", "a.txt"]);
        assert_eq!(tree.entries[0].entry_type, IdeEntryType::Dir);
    }

    #[tokio::test]
    async fn ide_file_reads_a_files_content() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.md"), b"hello world").unwrap();
        let engine = engine(&dir);
        let file = engine.ide_file("notes.md").await.unwrap();
        assert_eq!(file.content, "hello world");
        assert_eq!(file.size, 11);
    }

    #[tokio::test]
    async fn ide_file_rejects_a_path_that_escapes_the_project() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.ide_file("../secret.txt").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn ide_file_reports_not_found_for_a_missing_file() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.ide_file("does-not-exist.txt").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn ide_save_overwrites_an_existing_files_content() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.md"), b"old").unwrap();
        let engine = engine(&dir);
        let saved = engine.ide_save("notes.md", "new content").await.unwrap();
        assert_eq!(saved.content, "new content");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "new content"
        );
    }

    #[tokio::test]
    async fn ide_save_cannot_create_a_file_that_does_not_already_exist() {
        // Matches the oracle exactly: `ide_write_file` resolves the target path (which requires
        // it to already exist) before writing — `PUT /ide/file` edits, it does not create.
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .ide_save("brand-new.md", "content")
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    // ---- per-repo config (C1 continuation) -------------------------------------------------

    #[tokio::test]
    async fn config_reports_defaults_when_no_config_file_exists() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let config = engine.config().await.unwrap();
        assert_eq!(config.max_parallel, 2);
        assert!(!config.models_locked);
        assert!(config.base_branch.is_none());
    }

    #[tokio::test]
    async fn put_config_persists_a_patch_and_a_later_read_reflects_it() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let input = SetConfigInput {
            base_branch: Some(Some("develop".to_owned())),
            max_parallel: Some(5),
            ..Default::default()
        };
        let updated = engine.put_config(&input).await.unwrap();
        assert_eq!(updated.base_branch.as_deref(), Some("develop"));
        assert_eq!(updated.max_parallel, 5);

        let reread = engine.config().await.unwrap();
        assert_eq!(reread.base_branch.as_deref(), Some("develop"));
        assert_eq!(reread.max_parallel, 5);
    }

    #[tokio::test]
    async fn put_config_clears_a_field_when_the_patch_sets_it_to_null() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine
            .put_config(&SetConfigInput {
                base_branch: Some(Some("develop".to_owned())),
                ..Default::default()
            })
            .await
            .unwrap();
        let cleared = engine
            .put_config(&SetConfigInput {
                base_branch: Some(None),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(cleared.base_branch.is_none());
    }

    #[tokio::test]
    async fn put_config_rejects_max_parallel_outside_one_to_sixteen() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .put_config(&SetConfigInput {
                max_parallel: Some(17),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn put_config_rejects_a_default_models_change_when_locked_by_the_repo_config() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai/coducktor")).unwrap();
        std::fs::write(
            dir.path().join(".ai/coducktor/config.json"),
            r#"{"modelsLocked": true}"#,
        )
        .unwrap();
        let engine = engine(&dir);
        let error = engine
            .put_config(&SetConfigInput {
                default_models: Some(coducktor_contract::RunnerModelsPatch {
                    claude: Some(Some("opus".to_owned())),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    // ---- repo/run git (C1 continuation) ----------------------------------------------------

    /// Mirrors `coducktor-core::git::worktree`'s own `fixture_repo()` test helper: tempdir →
    /// `git init -q -b main` → commit a base file with an explicit test identity.
    fn fixture_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let ok = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        ok(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        ok(&["add", "-A"]);
        ok(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-q",
            "-m",
            "base",
        ]);
        dir
    }

    fn commit_all_git(root: &Path, message: &str) {
        assert!(
            Command::new("git")
                .current_dir(root)
                .args(["add", "-A"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(root)
                .args([
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@local",
                    "commit",
                    "-q",
                    "-m",
                    message
                ])
                .status()
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn repo_reports_present_for_a_real_git_repository() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let repo = engine.repo().await.unwrap();
        match repo {
            RepoResponse::Present(present) => {
                assert_eq!(present.info.branch, "main");
                assert!(present.log.iter().any(|entry| entry.subject == "base"));
            }
            RepoResponse::Empty(_) => panic!("expected Present"),
        }
    }

    #[tokio::test]
    async fn repo_reports_empty_for_a_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let repo = engine.repo().await.unwrap();
        assert!(matches!(repo, RepoResponse::Empty(_)));
    }

    #[tokio::test]
    async fn repo_changes_lists_a_modified_tracked_file_against_head() {
        let dir = fixture_repo();
        std::fs::write(dir.path().join("base.txt"), "changed\n").unwrap();
        let engine = engine(&dir);
        let changes = engine.repo_changes().await.unwrap();
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].path, "base.txt");
        assert_eq!(changes.files[0].status, ChangedFileStatus::Modified);
    }

    #[tokio::test]
    async fn repo_commit_returns_a_structured_payload_for_a_known_sha() {
        let dir = fixture_repo();
        let sha = String::from_utf8(
            Command::new("git")
                .current_dir(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let engine = engine(&dir);
        let payload = engine.repo_commit(&sha).await.unwrap();
        assert_eq!(payload.sha, sha);
        assert_eq!(payload.subject, "base");
    }

    #[tokio::test]
    async fn repo_commit_rejects_a_malformed_sha() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let error = engine.repo_commit("not-a-sha!!").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn repo_branch_creates_and_checks_out_a_new_branch() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let response = engine
            .repo_branch(&RepoBranchRequest {
                name: "feature/x".to_owned(),
                from: None,
            })
            .await
            .unwrap();
        assert!(response.created);
        assert_eq!(response.branch, "feature/x");
        let current = String::from_utf8(
            Command::new("git")
                .current_dir(dir.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(current.trim(), "feature/x");
    }

    #[tokio::test]
    async fn repo_branch_rejects_an_unsafe_branch_name() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let error = engine
            .repo_branch(&RepoBranchRequest {
                name: "--evil".to_owned(),
                from: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn run_files_lists_the_repo_root_when_the_run_has_no_worktree() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("look around")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        let files = engine.run_files(&run.id, None).await.unwrap();
        match files {
            WorktreeEntry::Dir { entries, .. } => {
                assert!(entries.iter().any(|entry| entry.name == "base.txt"));
            }
            WorktreeEntry::File { .. } => panic!("expected Dir"),
        }
    }

    #[tokio::test]
    async fn run_changes_lists_a_modification_relative_to_the_runs_base_branch() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("look around")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        commit_all_git(dir.path(), "second"); // moves HEAD past the run's implicit base
        std::fs::write(dir.path().join("base.txt"), "changed again\n").unwrap();
        let changes = engine.run_changes(&run.id).await.unwrap();
        assert!(changes.files.iter().any(|file| file.path == "base.txt"));
    }

    #[tokio::test]
    async fn run_diff_text_and_run_commit_and_run_files_report_not_found_for_an_unknown_run() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.run_diff_text("no-such-run").await.unwrap_err(),
            EngineError::NotFound
        );
        assert_eq!(
            engine.run_files("no-such-run", None).await.unwrap_err(),
            EngineError::NotFound
        );
        assert_eq!(
            engine
                .run_commit("no-such-run", "deadbeef")
                .await
                .unwrap_err(),
            EngineError::NotFound
        );
    }

    #[tokio::test]
    async fn run_file_raw_rejects_a_non_image_file() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("look around")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        let error = engine.run_file_raw(&run.id, "base.txt").await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[test]
    fn changed_file_status_maps_the_git_status_letters() {
        assert_eq!(changed_file_status("A"), ChangedFileStatus::Added);
        assert_eq!(changed_file_status("D"), ChangedFileStatus::Deleted);
        assert_eq!(changed_file_status("R100"), ChangedFileStatus::Renamed);
        assert_eq!(changed_file_status("C50"), ChangedFileStatus::Copied);
        assert_eq!(changed_file_status("M"), ChangedFileStatus::Modified);
    }

    #[test]
    fn valid_commit_hash_accepts_hex_strings_of_a_plausible_length() {
        assert!(valid_commit_hash("abcd"));
        assert!(valid_commit_hash(&"a".repeat(40)));
        assert!(!valid_commit_hash("abc"));
        assert!(!valid_commit_hash(&"a".repeat(41)));
        assert!(!valid_commit_hash("not-hex!"));
    }

    #[test]
    fn image_content_type_recognizes_common_extensions_and_falls_back_to_octet_stream() {
        assert_eq!(image_content_type(Path::new("a.png")), "image/png");
        assert_eq!(image_content_type(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(
            image_content_type(Path::new("a.txt")),
            "application/octet-stream"
        );
    }

    #[test]
    fn contains_git_component_detects_a_git_segment_anywhere_in_the_path() {
        assert!(contains_git_component(".git/config"));
        assert!(contains_git_component("src/.git/hooks/pre-commit"));
        assert!(!contains_git_component("src/gitignore.txt"));
    }

    // ---- agent-config (C1 continuation) ----------------------------------------------------
    // Tests below only exercise project/local-scoped definitions (resolved under the tempdir
    // repo root) — user-scoped definitions resolve against the REAL `agent_home_paths`, and
    // writing to a real environment's `~/.claude` etc. from a test is out of bounds, same
    // precedent C1.3 already established for the agent-accounts family.

    #[tokio::test]
    async fn agent_config_lists_every_definition_with_a_user_mcp_listing() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let listing = engine.agent_config().await.unwrap();
        assert_eq!(listing.files.len(), AGENT_CONFIG_DEFINITIONS.len());
        assert!(listing.editable);
        assert!(listing.user_mcp.is_some());
    }

    #[tokio::test]
    async fn agent_config_file_reports_not_found_for_an_unknown_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.agent_config_file("nonsense.id").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn agent_config_file_reports_a_missing_project_file_as_not_existing() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let content = engine
            .agent_config_file("claude.project.settings")
            .await
            .unwrap();
        assert!(!content.exists);
        assert!(content.version.is_none());
    }

    #[tokio::test]
    async fn put_agent_config_file_creates_a_project_file_and_a_later_read_reflects_it() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let written = engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: "{}".to_owned(),
                    version: None,
                },
            )
            .await
            .unwrap();
        assert!(written.exists);
        assert_eq!(written.content, "{}");

        let reread = engine
            .agent_config_file("claude.project.settings")
            .await
            .unwrap();
        assert_eq!(reread.content, "{}");
        assert_eq!(reread.version, written.version);
    }

    #[tokio::test]
    async fn put_agent_config_file_rejects_invalid_json_content() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: "{not json".to_owned(),
                    version: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn put_agent_config_file_rejects_a_stale_version() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: "{}".to_owned(),
                    version: None,
                },
            )
            .await
            .unwrap();
        let error = engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: r#"{"a":1}"#.to_owned(),
                    version: Some("stale".to_owned()),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn put_agent_config_file_refuses_to_empty_a_nonempty_file() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let written = engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: r#"{"a":1}"#.to_owned(),
                    version: None,
                },
            )
            .await
            .unwrap();
        let error = engine
            .put_agent_config_file(
                "claude.project.settings",
                &SetAgentConfigInput {
                    content: String::new(),
                    version: written.version,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[test]
    fn validate_agent_config_accepts_valid_and_rejects_malformed_content() {
        assert!(validate_agent_config("{}", AgentConfigFormat::Json).is_ok());
        assert!(validate_agent_config("{not json", AgentConfigFormat::Json).is_err());
        assert!(validate_agent_config("a = 1", AgentConfigFormat::Toml).is_ok());
        assert!(validate_agent_config("a = [", AgentConfigFormat::Toml).is_err());
        assert!(validate_agent_config("// comment\n{}", AgentConfigFormat::JsonC).is_ok());
        assert!(validate_agent_config("anything at all", AgentConfigFormat::Markdown).is_ok());
        assert!(validate_agent_config("", AgentConfigFormat::Json).is_ok());
    }

    #[test]
    fn jsonc_without_comments_strips_line_and_block_comments_but_not_string_content() {
        let input = "{\n  // a comment\n  \"a\": 1, /* block */ \"b\": \"// not a comment\"\n}";
        let stripped = jsonc_without_comments(input);
        assert!(serde_json::from_str::<Value>(&stripped).is_ok());
        assert!(stripped.contains("// not a comment"));
    }

    #[test]
    fn config_hash_is_deterministic_and_content_sensitive() {
        assert_eq!(config_hash(b"same"), config_hash(b"same"));
        assert_ne!(config_hash(b"a"), config_hash(b"b"));
    }

    // ---- worktree management (C1 continuation) ---------------------------------------------

    #[tokio::test]
    async fn worktrees_reports_empty_when_no_run_has_a_worktree() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let CreateRunResponse::Single(_run) =
            engine.start_run(steps_input("no worktree")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        let worktrees = engine.worktrees().await.unwrap();
        assert!(worktrees.worktrees.is_empty());
        assert_eq!(worktrees.total_bytes, Some(0));
    }

    #[tokio::test]
    async fn reclaim_worktrees_reports_no_reclaimed_ids_when_nothing_has_a_worktree() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let reclaimed = engine.reclaim_worktrees().await.unwrap();
        assert!(reclaimed.reclaimed.is_empty());
    }

    #[tokio::test]
    async fn remove_run_worktree_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.remove_run_worktree("no-such-run").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn remove_run_worktree_succeeds_trivially_for_a_finished_run_with_no_worktree() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let CreateRunResponse::Single(run) =
            engine.start_run(steps_input("no worktree")).await.unwrap()
        else {
            panic!("expected a single run");
        };
        let response = engine.remove_run_worktree(&run.id).await.unwrap();
        assert!(response.removed);
    }

    #[test]
    fn worktree_run_status_maps_every_run_status_variant() {
        use coducktor_contract::RunStatus;
        assert_eq!(
            worktree_run_status(RunStatus::Queued),
            WorktreeRunStatus::Queued
        );
        assert_eq!(
            worktree_run_status(RunStatus::Running),
            WorktreeRunStatus::Running
        );
        assert_eq!(
            worktree_run_status(RunStatus::Waiting),
            WorktreeRunStatus::Waiting
        );
        assert_eq!(
            worktree_run_status(RunStatus::Review),
            WorktreeRunStatus::Review
        );
        assert_eq!(
            worktree_run_status(RunStatus::Done),
            WorktreeRunStatus::Done
        );
        assert_eq!(
            worktree_run_status(RunStatus::Failed),
            WorktreeRunStatus::Failed
        );
        assert_eq!(
            worktree_run_status(RunStatus::Cancelled),
            WorktreeRunStatus::Cancelled
        );
    }

    #[test]
    fn worktree_size_bytes_sums_nested_directory_contents() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"1234").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"12345678").unwrap();
        assert_eq!(worktree_size_bytes(dir.path()), Some(12));
    }

    // ---- open-targets (mirrors coducktor-server's
    // `open_target_routes_list_local_apps_and_reject_project_cli_handoffs`) --------------------

    #[tokio::test]
    async fn open_targets_always_lists_the_file_manager_and_terminal_first() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.open_targets().await.unwrap();
        assert_eq!(response.targets[0].id, "finder");
        assert_eq!(response.targets[1].id, "terminal");
    }

    #[tokio::test]
    async fn open_project_in_rejects_an_empty_target() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.open_project_in("").await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "target required".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn open_project_in_rejects_an_overlong_target() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let overlong = "x".repeat(201);
        let error = engine.open_project_in(&overlong).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "target required".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn open_project_in_rejects_agent_cli_handoffs() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.open_project_in("cli:claude").await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "agent CLIs open a task worktree, not the project folder".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn open_project_in_rejects_an_app_not_present_on_this_machine() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.open_project_in("missing-editor").await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "no such app on this machine: missing-editor".to_owned()
            }
        );
    }

    #[test]
    fn executable_on_path_rejects_an_empty_binary_name() {
        assert!(!executable_on_path(""));
    }

    #[test]
    fn executable_on_path_rejects_a_binary_that_does_not_exist_anywhere_on_path() {
        assert!(!executable_on_path("coducktor-test-nonexistent-binary-xyz"));
    }

    #[test]
    fn open_target_command_returns_none_for_an_unrecognized_target() {
        let dir = TempDir::new().unwrap();
        assert!(open_target_command("not-a-real-target", dir.path()).is_none());
    }

    #[test]
    fn open_target_command_points_finder_and_terminal_at_the_repo_root_on_every_platform() {
        let dir = TempDir::new().unwrap();
        for target in ["finder", "terminal"] {
            let (_program, args) = open_target_command(target, dir.path())
                .unwrap_or_else(|| panic!("{target} should always resolve to a command"));
            assert!(
                args.iter().any(|arg| arg == &dir.path().to_string_lossy()),
                "{target}'s args should carry the repo root: {args:?}"
            );
        }
    }

    // ---- variant groups (mirrors coducktor-server's
    // `group_routes_compare_variants_and_archive_losers_on_pick`) -----------------------------

    fn seed_group(engine: &InProcessEngine, group_id: &str) -> Vec<String> {
        let mut manager = engine.manager.lock().unwrap();
        let mut ids = Vec::new();
        for (variant, title) in [("A", "first"), ("B", "second")] {
            let run = manager
                .create_run(coducktor_core::workflows::run::CreateRunInput {
                    title: title.to_owned(),
                    workflow: "manual".to_owned(),
                    task: title.to_owned(),
                    group_id: Some(group_id.to_owned()),
                    variant: Some(variant.to_owned()),
                    ..coducktor_core::workflows::run::CreateRunInput::default()
                })
                .expect("seed variant");
            ids.push(run.id);
        }
        ids
    }

    #[tokio::test]
    async fn group_reports_not_found_for_an_unknown_group() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.group("no-such-group").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn group_lists_every_variant_sorted_and_with_no_diff_stat_for_a_worktree_less_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let ids = seed_group(&engine, "group-1");
        let response = engine.group("group-1").await.unwrap();
        assert_eq!(response.group_id, "group-1");
        assert_eq!(response.runs.len(), 2);
        assert_eq!(response.runs[0].id, ids[0]);
        assert_eq!(response.runs[0].variant, "A");
        assert_eq!(response.runs[1].variant, "B");
        assert!(response.runs[0].diff_stat.is_empty());
    }

    #[tokio::test]
    async fn pick_variant_rejects_a_blank_run_id() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        seed_group(&engine, "group-1");
        let error = engine
            .pick_variant(
                "group-1",
                &PickVariantRequest {
                    run_id: String::new(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "runId is required".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn pick_variant_reports_not_found_for_an_unknown_group() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .pick_variant(
                "no-such-group",
                &PickVariantRequest {
                    run_id: "whatever".to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn pick_variant_rejects_a_run_id_outside_the_group() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        seed_group(&engine, "group-1");
        let error = engine
            .pick_variant(
                "group-1",
                &PickVariantRequest {
                    run_id: "not-in-this-group".to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "runId is not part of this group".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn pick_variant_archives_the_losers_and_keeps_the_winner_unarchived() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let ids = seed_group(&engine, "group-1");
        let winner_id = ids[0].clone();
        let response = engine
            .pick_variant(
                "group-1",
                &PickVariantRequest {
                    run_id: winner_id.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(response.winner.map(|run| run.id), Some(winner_id.clone()));

        let group = engine.group("group-1").await.unwrap();
        let winner = group.runs.iter().find(|run| run.id == winner_id).unwrap();
        let loser = group.runs.iter().find(|run| run.id != winner_id).unwrap();
        assert!(!winner.archived);
        assert!(loser.archived);
    }

    // ---- host model catalog (`models`) -----------------------------------------------------

    #[test]
    fn opencode_model_catalog_parser_preserves_order_and_rejects_banners() {
        let models = parse_opencode_models(
            "openai/gpt-5\n\u{1b}[32manthropic/claude-sonnet-4\u{1b}[0m\nopenai/gpt-5\n",
        )
        .expect("valid model listing");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "openai/gpt-5");
        assert_eq!(models[1].description, "via anthropic");
        assert!(parse_opencode_models("warning: no models\n").is_err());
    }

    #[tokio::test]
    async fn models_rejects_a_runner_with_no_discovery_path() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        for runner in [Runner::Claude, Runner::Pi] {
            let error = engine.models(runner).await.unwrap_err();
            assert_eq!(
                error,
                EngineError::Conflict {
                    reason: "runner must be codex or opencode".to_owned()
                }
            );
        }
    }

    #[tokio::test]
    async fn models_falls_back_to_unavailable_when_the_cli_cannot_be_spawned() {
        // No `codex`/`opencode` binary is installed in this sandbox (same assumption this
        // repo's other backend tests already document, e.g. B9a's runner tests) — live
        // discovery fails to spawn, and with no prior cache entry the result is `Unavailable`
        // with a reason, never an `Err` (a missing CLI degrades gracefully, matching the
        // `coducktor-server` oracle's own `get_models` handler).
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.models(Runner::Codex).await.unwrap();
        assert_eq!(response.runner, Runner::Codex);
        assert!(response.models.is_empty());
        assert_eq!(response.source, ModelCatalogSource::Unavailable);
        assert!(response.reason.is_some());
    }

    #[tokio::test]
    async fn models_serves_a_live_cache_entry_within_its_ttl_without_reprobing() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        {
            let mut cache = engine.model_catalog.lock().unwrap();
            cache.push(CachedModelCatalog {
                runner: ModelDiscoveryRunner::OpenCode,
                models: vec![RunnerModelOption {
                    id: "openai/gpt-5".to_owned(),
                    label: "openai/gpt-5".to_owned(),
                    description: "via openai".to_owned(),
                    reasoning_efforts: None,
                }],
                expires_at: Instant::now() + Duration::from_secs(60),
                failure_reason: None,
            });
        }
        let response = engine.models(Runner::OpenCode).await.unwrap();
        assert_eq!(response.source, ModelCatalogSource::Cache);
        assert_eq!(response.models.len(), 1);
        assert_eq!(response.models[0].id, "openai/gpt-5");
        assert!(!response.stale);
    }

    #[tokio::test]
    async fn models_reprobes_once_a_cache_entry_expires() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        {
            let mut cache = engine.model_catalog.lock().unwrap();
            cache.push(CachedModelCatalog {
                runner: ModelDiscoveryRunner::Codex,
                models: Vec::new(),
                // Already expired — the TTL check must re-probe rather than serve this.
                expires_at: Instant::now() - Duration::from_secs(1),
                failure_reason: None,
            });
        }
        let response = engine.models(Runner::Codex).await.unwrap();
        // Re-probed (the sandbox has no `codex` binary), so this is a fresh `Unavailable`
        // result, not the stale expired cache entry served as-is.
        assert_eq!(response.source, ModelCatalogSource::Unavailable);
    }

    // ---- plan ---------------------------------------------------------------------------

    #[tokio::test]
    async fn plan_rejects_an_empty_or_oversized_task() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        for task in ["", "   ", &"x".repeat(100_001)] {
            let error = engine.plan(task).await.unwrap_err();
            assert_eq!(
                error,
                EngineError::Conflict {
                    reason: "task must be between 1 and 100000 characters".to_owned()
                }
            );
        }
    }

    #[tokio::test]
    async fn plan_returns_the_safe_single_step_fallback() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.plan("build a widget").await.unwrap();
        assert!(response.fallback);
        assert_eq!(response.steps.len(), 1);
        assert_eq!(response.steps[0].prompt.as_deref(), Some("{{task}}"));
    }

    // ---- GitHub forge --------------------------------------------------------------------
    // `fixture_repo()` has no `origin` remote, so every real call below exercises the
    // `GithubDriver`-unavailable degrade path — the same "no GitHub configured" state most
    // task worktrees are in. A live `gh`-backed round trip is out of scope for a unit suite;
    // `coducktor-forge`'s own tests (B7) already cover `GithubDriver` itself with an injected
    // command/GraphQL seam.

    #[tokio::test]
    async fn github_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let data = engine.github().await.unwrap();
        assert!(!data.available);
        assert_eq!(
            data.reason.as_deref(),
            Some("GitHub is unavailable for this repository")
        );
    }

    #[tokio::test]
    async fn github_checks_rejects_an_empty_or_oversized_prs_list() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.github_checks(&[]).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid prs query".to_owned()
            }
        );
        let too_many: Vec<String> = (1..=101).map(|n| n.to_string()).collect();
        assert_eq!(
            engine.github_checks(&too_many).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid prs query".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_checks_rejects_a_non_numeric_pr() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let error = engine
            .github_checks(&["12".to_owned(), "not-a-number".to_owned()])
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "invalid prs query".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_checks_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let data = engine.github_checks(&["12".to_owned()]).await.unwrap();
        match data {
            GithubChecksData::Unavailable(unavailable) => {
                assert!(!unavailable.available);
                assert_eq!(
                    unavailable.reason,
                    "GitHub is unavailable for this repository"
                );
            }
            GithubChecksData::Available(_) => panic!("expected Unavailable"),
        }
    }

    #[tokio::test]
    async fn github_ref_status_rejects_a_missing_query() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.github_ref_status(&[], &[]).await.unwrap_err(),
            EngineError::Conflict {
                reason: "missing prs or issues query".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_ref_status_reports_unavailable_without_an_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let prs = vec!["1".to_owned()];
        let issues = Vec::new();
        assert_eq!(
            engine.github_ref_status(&prs, &issues).await.unwrap(),
            GithubRefStatusData::Unavailable(GithubRefStatusUnavailable {
                available: false,
                reason: "GitHub is unavailable for this repository".to_owned(),
                recheck_after_ms: None,
            })
        );
    }

    #[tokio::test]
    async fn github_comments_rejects_an_invalid_kind_or_number() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.github_comments("bogus", 1).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid kind or number".to_owned()
            }
        );
        assert_eq!(
            engine.github_comments("pr", 0).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid kind or number".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_comments_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let data = engine.github_comments("pr", 12).await.unwrap();
        assert!(!data.available);
        assert_eq!(
            data.reason.as_deref(),
            Some("GitHub is unavailable for this repository")
        );
    }

    #[tokio::test]
    async fn github_pr_merge_state_rejects_pr_number_zero() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.github_pr_merge_state(0).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid pull request number".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_pr_merge_state_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let response = engine.github_pr_merge_state(12).await.unwrap();
        match response {
            GithubPrMergeStateResponse::Unavailable { available, reason } => {
                assert!(!available);
                assert_eq!(reason, "GitHub is unavailable for this repository");
            }
            GithubPrMergeStateResponse::Available { .. } => panic!("expected Unavailable"),
        }
    }

    fn merge_input(sha: &str) -> GithubMergeInput {
        GithubMergeInput {
            method: coducktor_contract::GithubMergeMethod::Merge,
            expected_head_sha: sha.to_owned(),
            override_rules: None,
        }
    }

    #[tokio::test]
    async fn github_merge_pr_rejects_pr_number_zero() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let sha = "a".repeat(40);
        assert_eq!(
            engine
                .github_merge_pr(0, &merge_input(&sha))
                .await
                .unwrap_err(),
            EngineError::Conflict {
                reason: "invalid pull request number".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_merge_pr_rejects_a_malformed_expected_head_sha() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        for sha in ["too-short", &"g".repeat(40), &"A".repeat(40)] {
            let error = engine
                .github_merge_pr(12, &merge_input(sha))
                .await
                .unwrap_err();
            assert_eq!(
                error,
                EngineError::Conflict {
                    reason: "invalid merge request".to_owned()
                }
            );
        }
    }

    #[tokio::test]
    async fn github_merge_pr_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let sha = "a".repeat(40);
        let error = engine
            .github_merge_pr(12, &merge_input(&sha))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "GitHub merge is unavailable".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_pr_changes_rejects_pr_number_zero() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        assert_eq!(
            engine.github_pr_changes(0).await.unwrap_err(),
            EngineError::Conflict {
                reason: "invalid pull request number or refresh flag".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn github_pr_changes_reports_unavailable_with_no_origin_remote() {
        let dir = fixture_repo();
        let engine = engine(&dir);
        let data = engine.github_pr_changes(12).await.unwrap();
        match data {
            GithubPrChangesData::Unavailable(unavailable) => {
                assert!(!unavailable.available);
                assert_eq!(
                    unavailable.reason,
                    "GitHub is unavailable for this repository"
                );
            }
            GithubPrChangesData::Available(_) => panic!("expected Unavailable"),
        }
    }

    // ---- remaining settings writes ---------------------------------------------------------
    // `workspace_config`/`workspace_ui_state` read the real host `~/.coducktor/` state (this
    // module has no injectable `EnvSource` seam to isolate it — `coducktor-core`'s own
    // `paths::test_env::FixedEnv` is `pub(crate)`, not exported), so only read paths and
    // validation-rejection paths (which return before any file I/O) are exercised here — the
    // same restraint this file's agent-accounts family already documents for the identical
    // reason. `put_workspace_ui_state` has no validation branch at all (it always writes), so
    // it is not called here for real at any input — calling it would mutate the developer's own
    // `~/.coducktor/ui-state.json`, which a unit test must never do.

    #[tokio::test]
    async fn workspace_config_reads_without_error() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine.workspace_config().await.unwrap();
    }

    #[tokio::test]
    async fn workspace_ui_state_reads_without_error() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        engine.workspace_ui_state().await.unwrap();
    }

    #[tokio::test]
    async fn put_workspace_config_rejects_a_relative_projects_dir() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let input = SetWorkspaceConfigInput {
            projects_dir: Some("relative/path".to_owned()),
            ..Default::default()
        };
        let error = engine.put_workspace_config(&input).await.unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn put_workspace_config_rejects_an_out_of_range_max_parallel() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let input = SetWorkspaceConfigInput {
            resources: Some(coducktor_contract::WorkspaceResourcesPatch {
                max_parallel: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = engine.put_workspace_config(&input).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "maxParallel must be an integer from 1 to 16".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn remove_project_reports_not_found_for_an_unregistered_project() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .remove_project("definitely-not-a-registered-project-id")
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn update_project_rejects_an_input_with_neither_field_set() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let input = UpdateProjectInput {
            max_parallel: None,
            tags: None,
        };
        let error = engine.update_project("anything", &input).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "specify maxParallel or tags".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn update_project_reports_not_found_for_an_unregistered_project() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let input = UpdateProjectInput {
            max_parallel: Some(Some(4)),
            tags: None,
        };
        let error = engine
            .update_project("definitely-not-a-registered-project-id", &input)
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    // ---- task-thread write paths ------------------------------------------------------------

    // -- pure helpers --

    #[test]
    fn valid_queued_text_enforces_the_100k_character_cap() {
        assert!(valid_queued_text("hello"));
        assert!(!valid_queued_text(&"x".repeat(100_001)));
    }

    #[test]
    fn folded_task_length_joins_task_and_queued_text_ignoring_blanks() {
        let messages = vec![
            coducktor_contract::QueuedMessage {
                id: "1".to_owned(),
                text: "  ".to_owned(),
                images: None,
                created_at: "now".to_owned(),
            },
            coducktor_contract::QueuedMessage {
                id: "2".to_owned(),
                text: "second".to_owned(),
                images: None,
                created_at: "now".to_owned(),
            },
        ];
        assert_eq!(
            folded_task_length("first", &messages),
            "first\n\nsecond".len()
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("it's fine"), "'it'\\''s fine'");
    }

    #[test]
    fn safe_session_id_rejects_path_and_shell_shaped_values() {
        assert!(safe_session_id("abc123.def_ghi-1"));
        assert!(!safe_session_id(""));
        assert!(!safe_session_id("../etc/passwd"));
        assert!(!safe_session_id("$(rm -rf /)"));
        assert!(!safe_session_id(&"a".repeat(201)));
    }

    #[test]
    fn cursor_round_trips_through_encode_and_decode() {
        let cursor = PageCursor {
            v: 1,
            kind: "page".to_owned(),
            direction: "older".to_owned(),
            file_size: 42,
            boundary_seq: 7,
        };
        let encoded = encode_cursor(&cursor);
        let decoded: PageCursor = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded, PageCursor { ..cursor });
    }

    #[test]
    fn decode_cursor_rejects_garbage() {
        let error: EngineError = decode_cursor::<PageCursor>("not base64!!").unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "invalid history cursor".to_owned()
            }
        );
        let error: EngineError = decode_cursor::<PageCursor>("").unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "invalid history cursor".to_owned()
            }
        );
    }

    // -- wired-through-the-engine paths (NotFound/validation, matching the established
    // restraint elsewhere in this file: exercise what returns before deep RunManager session
    // state is needed — RunManager's own continue/session semantics are coducktor-core's own
    // test responsibility, this suite only proves the wiring) --

    #[tokio::test]
    async fn send_message_rejects_blank_text() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .send_message(
                &run.id,
                MessageInput {
                    text: Some("   ".to_owned()),
                    images: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "message needs text or at least one image".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn send_message_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .send_message(
                "no-such-run",
                MessageInput {
                    text: Some("hi".to_owned()),
                    images: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn continue_run_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine
            .continue_run("no-such-run", ContinueInput::default())
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn edit_queued_message_reports_not_found_when_the_run_has_no_queue() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .edit_queued_message(
                &run.id,
                "msg-1",
                QueuedMessagePatchInput {
                    text: Some("edited".to_owned()),
                    images: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn edit_queued_message_rejects_an_empty_patch() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .edit_queued_message(
                &run.id,
                "msg-1",
                QueuedMessagePatchInput {
                    text: None,
                    images: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "message edit needs text or images".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn remove_queued_message_reports_not_found_when_the_run_has_no_queue() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .remove_queued_message(&run.id, "msg-1")
            .await
            .unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn cancel_auto_resume_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.cancel_auto_resume("no-such-run").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn cancel_auto_resume_reports_cancelled_for_a_real_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let response = engine.cancel_auto_resume(&run.id).await.unwrap();
        assert!(response.cancelled);
    }

    #[tokio::test]
    async fn git_commit_reports_no_worktree_for_a_worktree_less_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .git_commit(
                &run.id,
                GitCommitInput {
                    message: "a commit".to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: NO_WORKTREE.to_owned()
            }
        );
    }

    #[tokio::test]
    async fn git_push_reports_no_worktree_for_a_worktree_less_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine.git_push(&run.id).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: NO_WORKTREE.to_owned()
            }
        );
    }

    #[tokio::test]
    async fn run_commits_reports_no_worktree_for_a_worktree_less_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        // `create_run` persists `worktree: Some(false)` when worktree creation was skipped in
        // this environment (the temp dir is not a real git repo) — `working_directory_of` reads
        // that as "ran directly in the repo working tree" and legitimately resolves to
        // `self.repo_root`, which is correct for a real worktree-less run but leaves nothing for
        // `run_commits`'s git shell-outs to work against here. Force the field to `None`
        // (genuinely never requested) so the NO_WORKTREE conflict this test actually means to
        // exercise is the one that fires, matching `create_pr`'s own equivalent test right below.
        {
            let mut manager = engine.manager.lock().unwrap();
            manager
                .update_run_value(&run.id, json!({ "worktree": null }))
                .unwrap();
        }
        let error = engine.run_commits(&run.id).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: NO_WORKTREE.to_owned()
            }
        );
    }

    #[tokio::test]
    async fn create_pr_reports_no_worktree_for_a_worktree_less_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine.create_pr(&run.id).await.unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "no worktree/branch to publish — this task ran in the repo working tree"
                    .to_owned()
            }
        );
    }

    #[tokio::test]
    async fn run_history_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.run_history("no-such-run", None).await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn run_history_reads_a_real_runs_events() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let page = engine.run_history(&run.id, None).await.unwrap();
        assert!(!page.events.is_empty());
    }

    #[tokio::test]
    async fn run_history_rejects_a_garbage_cursor() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .run_history(&run.id, Some("not a cursor"))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "invalid history cursor".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn run_history_context_reports_not_found_for_an_unknown_run() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.run_history_context("no-such-run").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn open_in_cli_reports_no_session_for_a_fresh_run_or_reports_not_found() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let error = engine.open_in_cli("no-such-run").await.unwrap_err();
        assert_eq!(error, EngineError::NotFound);
    }

    #[tokio::test]
    async fn open_in_rejects_an_empty_target() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .open_in(
                &run.id,
                OpenInInput {
                    target: "  ".to_owned(),
                    path: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "target required".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn open_in_rejects_an_unknown_cli_provider() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        let response = engine.start_run(steps_input("do the thing")).await.unwrap();
        let CreateRunResponse::Single(run) = response else {
            panic!("expected a single run");
        };
        let error = engine
            .open_in(
                &run.id,
                OpenInInput {
                    target: "cli:nonsense".to_owned(),
                    path: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            EngineError::Conflict {
                reason: "unknown target".to_owned()
            }
        );
    }
}
