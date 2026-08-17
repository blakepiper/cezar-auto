//! `InProcessEngine` — an `Engine` implementation that calls straight into `coducktor-core`
//! (and, for the families that need them, `coducktor-runners`/`coducktor-forge`) instead of
//! making an HTTP request. Spec §12/plan C1: because the `Engine` trait predates the server
//! (A2), this is meant to be "an implementation, not an extraction" — in practice a large
//! fraction of `coducktor-server`'s own handlers turned out to hold real business logic
//! directly (git shelling, IDE file I/O, agent-config file listing, provider probing) rather
//! than being thin `cezar-core` delegates the way that crate's own module doc promises, so
//! porting the *whole* `Engine` trait honestly is a bigger lift than this one step's text
//! implies. See this crate's module doc / the plan's C1 entry for exactly which families are
//! implemented here and which are deliberately left for a follow-up.
//!
//! **Status: partial.** `InProcessEngine` is a real, tested struct with working async methods
//! for the families listed below — but it does NOT yet `impl Engine`, on purpose: the trait
//! has ~85 methods, several families (IDE, repo git browsing, agent-config, provider/account
//! probing, GitHub forge detail reads, worktree management, open-targets, diff/compare,
//! settings write paths) are not ported yet, and claiming the full trait with `Err`-stub
//! methods for those would be a worse outcome than an honest partial. A follow-up step
//! finishes the remaining families and closes the trait impl.
//!
//! Every method here cites the `coducktor-server` handler it was ported from (that crate is
//! this port's oracle, the same role `packages/cezar` played for the rest of Phase B) —
//! `coducktor-server` is deleted whole at C2, so duplicating its business logic here now
//! (rather than trying to share it across an axum-shaped and a non-axum-shaped caller) is the
//! right amount of engineering, not a shortcut.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use coducktor_contract::{
    AgentAccountDetailsResponse, AgentAccountFile, AgentAccountStatusResponse, AgentConfigFile,
    AgentConfigFileContent, AgentConfigFormat, AgentConfigKind, AgentConfigListing,
    AgentConfigScope, AgentConfigTracked, AgentProfile, AgentProfileResponse,
    AgentProfileSelectionsResponse, AgentProfilesResponse, ApiRun, ArchiveFinishedResponse,
    BackendCheck, BackendCheckName, CancelResponse, Capabilities, ChangedFile, ChangedFileStatus,
    ChangesPayload, ConfigResponse, CreateAgentProfileInput, CreateRunInput, CreateRunResponse,
    DeleteRunResponse, DeleteWorkflowResponse, EmptyRepoResponse, FinishResponse, ForgeInfo,
    ForgeKind, HealthProject, HealthResponse, IdeDirectoryResponse, IdeEntry, IdeEntryType,
    IdeFileResponse, LogEntry, MarkAllReadResponse, OpenAgentAccountFileInput,
    OpenAgentAccountFileResponse, OpenProjectInResponse, OpenTargetsResponse, ParsedWorkflow,
    PatchRunInput, PresentRepoResponse, ProjectListEntry, ProjectSource, ProjectStatus,
    ProjectsResponse, ProviderConnectionState, ProviderStatus, ProviderStatusResponse,
    ReclaimWorktreesResponse, RemoveAgentProfileResponse, RemoveTodoResponse,
    RemoveWorktreeResponse, RepoBranchRequest, RepoBranchResponse, RepoCommitPayload, RepoDiffStat,
    RepoInfo, RepoResponse, RunIndexEntry, Runner, RunnerSelection, RunsIndexResponse,
    SaveWorkflowInput, SaveWorkflowResponse, SelectAgentProfileInput, SetAgentConfigInput,
    SetConfigInput, Skill, StartTodoResponse, StatusEntry, TodoItem, UpdateAgentProfileInput,
    UserMcpListing, WorkflowStepDef, WorkflowsResponse, WorkspaceUsageResponse, WorktreeDirEntry,
    WorktreeEntry, WorktreeEntryType, WorktreeInfo, WorktreeRunStatus, WorktreesResponse,
};
use coducktor_core::handoff::followups_enabled;
use coducktor_core::paths::{
    ProcessEnv, agent_accounts_path, agent_home_paths, expand_tilde, is_absolute_config_dir,
    real_home_dir,
};
use coducktor_core::skills::discover_skills;
use coducktor_core::workflows::load::{WORKFLOWS_DIR, load_workflows};
use coducktor_core::workflows::run::{RunManager, StartRunInput as CoreStartRunInput};
use coducktor_core::workflows::types::{parse_workflow_file_doc, quick_task_workflow, steps_issue};
use coducktor_core::workspace::agent_accounts::{
    AgentAccount, has_control_chars, is_valid_account_id, merge_write_agent_accounts,
    supports_profiles,
};
use coducktor_core::workspace::config::{PROVIDER_IDS, load_workspace_config};
use coducktor_runners::session_factory::DefaultSessionFactory;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;

use crate::Topic;
use crate::error::EngineError;
use crate::ws::EngineEvent;

/// Version string this engine reports through `health()` — set once at construction, same as
/// `coducktor-server`'s `ServerConfig::version`.
pub struct InProcessEngine {
    repo_root: PathBuf,
    version: String,
    manager: Arc<Mutex<RunManager>>,
    live_events: broadcast::Sender<EngineEvent>,
}

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
            let event = EngineEvent {
                topic: format!("run:{}", run.id),
                data: json!({ "type": "run", "run": run }),
            };
            let _ = run_sender.send(event);
        });

        Self {
            repo_root,
            version: version.into(),
            manager: Arc::new(Mutex::new(manager)),
            live_events,
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    // ---- health (ported from `coducktor-server::health_payload`/`backend_check`) ----------

    pub async fn health(&self) -> Result<HealthResponse, EngineError> {
        let repo_root = self.repo_root.clone();
        let version = self.version.clone();
        tokio::task::spawn_blocking(move || health_payload(&repo_root, &version))
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

    /// A reduced port of `start_todo`: runs the todo's own suggested skill (or a bare
    /// quick-task) with its saved prompt. Explicit `runner`/`model` overrides from the HTTP
    /// body are not threaded through yet — `StartTodoInput`'s fields are not exposed on this
    /// method's signature (a follow-up if a screen needs them before the trait is closed).
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
    /// `HttpEngine` but has no effect — the oracle's own handler ignores it too (there is no
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

    /// Ported from `open_agent_profile_file`, minus explicit-app-target selection: that depends
    /// on the not-yet-ported open-targets registry (`open_targets`/`open_target`, its own
    /// family — a C1 follow-up). `target: None` (open with the OS default opener) behaves
    /// exactly like the oracle; an explicit `target` is a clear `Conflict`, not a silent no-op.
    pub async fn open_agent_account_file(
        &self,
        id: &str,
        input: &OpenAgentAccountFileInput,
    ) -> Result<OpenAgentAccountFileResponse, EngineError> {
        if let Some(target) = input.target.as_deref() {
            return Err(EngineError::Conflict {
                reason: format!(
                    "opening with a specific target (\"{target}\") is not yet supported by \
                     InProcessEngine — the open-targets family is a C1 follow-up"
                ),
            });
        }
        let accounts_path = agent_accounts_path(&ProcessEnv);
        let id = id.to_owned();
        let file = input.file.clone();
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
            if !account_open_default(&path) {
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

    /// Mirrors `HttpEngine::subscribe`'s topic-string convention exactly, but the transport is
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
    // `pub`). `group`/`pick_variant` are a separate, more involved cluster (they mutate run
    // state — cancel/archive losing variants, remove their worktrees, touch the review gate) and
    // are deliberately left for a follow-up round rather than folded in alongside this batch.

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
    let duck_name = format!("DUCK_{}_BIN", provider.to_ascii_uppercase());
    let cez_name = format!("CEZ_{}_BIN", provider.to_ascii_uppercase());
    std::env::var(&duck_name)
        .ok()
        .or_else(|| std::env::var(&cez_name).ok())
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
            if !name.starts_with("cez/") && !name.starts_with("duck/") {
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
    std::env::var("CEZ_AGENT_MODELS_LOCKED").is_ok_and(|value| value == "1")
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
    std::env::var("DUCK_AGENT_MODELS_LOCKED")
        .or_else(|_| std::env::var("CEZ_AGENT_MODELS_LOCKED"))
        .is_ok_and(|value| value == "1")
}

fn provider_executable(provider: Runner) -> String {
    let (duck, cez, default) = match provider {
        Runner::Claude => ("DUCK_CLAUDE_BIN", "CEZ_CLAUDE_BIN", "claude"),
        Runner::Codex => ("DUCK_CODEX_BIN", "CEZ_CODEX_BIN", "codex"),
        Runner::OpenCode => ("DUCK_OPENCODE_BIN", "CEZ_OPENCODE_BIN", "opencode"),
        Runner::Pi => ("DUCK_PI_BIN", "CEZ_PI_BIN", "pi"),
    };
    std::env::var(duck)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var(cez)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
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
    if std::env::var("DUCK_DRY_RUN")
        .or_else(|_| std::env::var("CEZ_DRY_RUN"))
        .is_ok_and(|value| value == "1")
    {
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
                "OpenCode keeps its login outside its config folder, so cezar cannot read it."
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

fn health_payload(repo_root: &Path, version: &str) -> HealthResponse {
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let branch = git_output(repo_root, &["branch", "--show-current"]);
    let remote = git_output(repo_root, &["config", "--get", "remote.origin.url"]);
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
        latest_version: None,
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
        .map(|(name, binary)| backend_check(name, binary))
        .collect(),
        default_runner: RunnerSelection::Auto,
        forge: Some(ForgeInfo {
            kind: ForgeKind::GitHub,
            available: None,
            reason: Some(
                "InProcessEngine's forge routes are not wired yet (C1 follow-up)".to_owned(),
            ),
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

    /// A session that immediately completes with `CEZ:DONE` — enough to prove
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
                    turn_text: "done (fake)\n\nCEZ:DONE".to_owned(),
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
        // CEZ_FOLLOWUPS/DUCK_FOLLOWUPS are unset in the test process by default.
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
    // `$DUCK_HOME`/`$CEZ_HOME` if set), with no injectable override. No test here calls one of
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
    async fn open_agent_account_file_rejects_an_explicit_target_before_touching_disk() {
        let dir = TempDir::new().unwrap();
        let engine = engine(&dir);
        // No account with this id exists either, but an explicit `target` must be rejected
        // first — proves the check happens before any lookup, not just before any I/O.
        let error = engine
            .open_agent_account_file(
                "coducktor-test-account-that-does-not-exist",
                &OpenAgentAccountFileInput {
                    file: "folder".to_owned(),
                    target: Some("vscode".to_owned()),
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
}
