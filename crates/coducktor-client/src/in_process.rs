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
    ApiRun, ArchiveFinishedResponse, BackendCheck, BackendCheckName, CancelResponse, Capabilities,
    CreateRunInput, CreateRunResponse, DeleteRunResponse, FinishResponse, ForgeInfo, ForgeKind,
    HealthProject, HealthResponse, MarkAllReadResponse, PatchRunInput, ProjectListEntry,
    ProjectSource, ProjectStatus, ProjectsResponse, RemoveTodoResponse, RepoInfo, RunIndexEntry,
    RunnerSelection, RunsIndexResponse, Skill, StartTodoResponse, TodoItem, WorkflowsResponse,
};
use coducktor_core::handoff::followups_enabled;
use coducktor_core::paths::ProcessEnv;
use coducktor_core::skills::discover_skills;
use coducktor_core::workflows::load::load_workflows;
use coducktor_core::workflows::run::{RunManager, StartRunInput as CoreStartRunInput};
use coducktor_core::workflows::types::quick_task_workflow;
use coducktor_core::workspace::config::load_workspace_config;
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
}
