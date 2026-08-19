use std::collections::HashSet;
use std::env;
use std::future::Future;
use std::io;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver as BackgroundReceiver, Sender as BackgroundSender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coducktor_client::{Engine, EngineEvent, InProcessEngine, Scope, Topic};
use coducktor_contract::{ApiRun, BackendCheckName, TaskSource};
use coducktor_core::paths::ProcessEnv;
use coducktor_core::workspace::migrations::run_migrations;
use crossterm::event::{self, Event, MouseEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

use crate::app::{self, App, PendingAction, WorkspaceEvent};
use crate::cli::{Cli, Command};
use crate::input::keymap::Keymap;
use crate::terminal::AppTerminal;
use crate::theme::Theme;
use crate::welcome::WelcomeAnimation;
use crate::{cli, headless, new_task_form, screens, terminal};

const FRAME_BUDGET: Duration = Duration::from_millis(33);
const INPUT_ITEMS_PER_FRAME: usize = 64;
const RECEIVER_ITEMS_PER_FRAME: usize = 256;
const RECEIVER_TIME_BUDGET: Duration = Duration::from_millis(4);
const PENDING_ACTIONS_PER_FRAME: usize = 16;
const BACKGROUND_WORKER_COUNT: usize = 4;

#[tokio::main]
pub async fn entry() -> io::Result<()> {
    let cli = Cli::parse_args();
    // A bad `--repo` is a startup misconfiguration, not a runtime event — reject it
    // before the alternate screen opens: the
    // TUI never took the screen, so there is nothing to restore.
    if let Some(repo) = &cli.repo
        && !repo.is_dir()
    {
        eprintln!("coducktor: --repo {} is not a directory", repo.display());
        std::process::exit(2);
    }
    let repo_root = headless::resolve_repo_root(cli.repo.as_deref());
    for message in run_migrations(Some(&repo_root), &ProcessEnv).messages {
        eprintln!("coducktor: {message}");
    }
    // The non-interactive subcommands never open the alternate screen — they run
    // straight in the caller's terminal, print to real stdout/stderr, and exit. Only
    // `None`/`Tui` fall through to the interactive cockpit below.
    match &cli.command {
        Some(Command::Run { task }) => {
            let code = headless::run_command(
                repo_root,
                task.join(" "),
                cli.workflow.clone(),
                cli.model.clone(),
            )
            .await;
            std::process::exit(code);
        }
        Some(Command::Init) => {
            headless::init_command(&repo_root);
            return Ok(());
        }
        Some(Command::Usage { json, refresh }) => {
            std::process::exit(headless::usage_command(repo_root, *json, *refresh).await);
        }
        Some(Command::Doctor { json }) => {
            std::process::exit(headless::doctor_command(repo_root, *json).await);
        }
        Some(Command::RepairRuns) => {
            std::process::exit(headless::repair_runs_command(&repo_root));
        }
        Some(Command::Projects { action }) => {
            std::process::exit(headless::projects_command(&repo_root, action.clone()));
        }
        None | Some(Command::Tui) => {}
    }
    terminal::install_panic_hook();
    let mut terminal = terminal::setup()?;
    let user_keymap = Keymap::default_path();
    let keymap = Keymap::load(user_keymap.as_deref()).unwrap_or_default();
    let mut app = App::new("main", Theme::detect(), keymap);
    app.set_boot_root(repo_root.clone());
    let engine: Arc<dyn Engine> =
        Arc::new(InProcessEngine::new(repo_root, env!("CARGO_PKG_VERSION")));
    let mut workspace_listener =
        open_workspace_listener(engine.clone(), app.current_project().to_owned()).await;
    let run_result = run(
        &mut terminal,
        &mut app,
        engine,
        workspace_listener.as_mut().map(|(_, receiver)| receiver),
        &cli,
    )
    .await;
    if let Some((handle, _)) = workspace_listener {
        handle.abort();
    }
    let restore_result = terminal::restore();

    run_result.and(restore_result)
}

fn parse_workspace_event(event: EngineEvent, fallback_project: &str) -> Option<WorkspaceEvent> {
    if event.data.get("type")?.as_str()? != "run" {
        return None;
    }
    let record = serde_json::from_value(event.data.get("run")?.clone()).ok()?;
    let project = event
        .data
        .get("projectId")
        .and_then(serde_json::Value::as_str)
        .filter(|project| !project.is_empty())
        .unwrap_or(fallback_project);
    Some(WorkspaceEvent::Run {
        project: project.to_owned(),
        run: ApiRun {
            record,
            usage: None,
        },
    })
}

struct PrimeSnapshot {
    health: Option<coducktor_contract::HealthResponse>,
    runs: Option<Vec<ApiRun>>,
    projects: Option<coducktor_contract::ProjectsResponse>,
    index: Option<coducktor_contract::RunsIndexResponse>,
    workspace_ui_state: Option<coducktor_contract::WorkspaceUiState>,
    new_task: PrimeNewTaskSnapshot,
}

struct PrimeNewTaskSnapshot {
    config: Option<coducktor_contract::ConfigResponse>,
    skills: Option<Vec<coducktor_contract::Skill>>,
    workflows: Option<coducktor_contract::WorkflowsResponse>,
    workspace_config: Option<coducktor_contract::WorkspaceConfigResponse>,
    provider_status: Option<coducktor_contract::ProviderStatusResponse>,
    ui_state: Option<coducktor_contract::UiState>,
    repo: Option<coducktor_contract::RepoInfo>,
    branches: Vec<String>,
}

struct SettingsSnapshot {
    config: Option<coducktor_contract::ConfigResponse>,
    workspace_config: Option<coducktor_contract::WorkspaceConfigResponse>,
    workspace_ui_state: Option<coducktor_contract::WorkspaceUiState>,
    ui_state: Option<coducktor_contract::UiState>,
    agent_config: Option<coducktor_contract::AgentConfigListing>,
    agent_profiles: Option<coducktor_contract::AgentProfilesResponse>,
    worktrees: Option<coducktor_contract::WorktreesResponse>,
}

#[allow(clippy::large_enum_variant)]
enum BackgroundResult {
    StartRun {
        project: String,
        result: Result<coducktor_contract::CreateRunResponse, coducktor_client::EngineError>,
    },
    ActivateRuns {
        result: Result<(), coducktor_client::EngineError>,
    },
    CreatePr {
        project: String,
        id: String,
        result: Result<coducktor_contract::CreatePrResponse, coducktor_client::EngineError>,
    },
    OpenInCli {
        result: Result<coducktor_contract::OpenInCliResponse, coducktor_client::EngineError>,
    },
    Github {
        project: String,
        result: Result<coducktor_contract::GithubData, coducktor_client::EngineError>,
    },
    GithubComments {
        project: String,
        number: u64,
        result: Result<coducktor_contract::GithubCommentsData, coducktor_client::EngineError>,
    },
    GithubMergeState {
        project: String,
        number: u64,
        result:
            Result<coducktor_contract::GithubPrMergeStateResponse, coducktor_client::EngineError>,
    },
    GithubPrChanges {
        project: String,
        number: u64,
        result: Result<coducktor_contract::GithubPrChangesData, coducktor_client::EngineError>,
    },
    GithubMerge {
        project: String,
        number: u64,
        result: Result<coducktor_contract::GithubMergeResponse, coducktor_client::EngineError>,
    },
    LoadThread {
        project: String,
        id: String,
        run: Result<coducktor_contract::ApiRun, coducktor_client::EngineError>,
        history: Result<coducktor_contract::RunHistoryPage, coducktor_client::EngineError>,
    },
    LoadEarlierThread {
        project: String,
        id: String,
        history: Result<coducktor_contract::RunHistoryPage, coducktor_client::EngineError>,
    },
    RefreshTasks {
        project: String,
        generation: u64,
        result: Result<Vec<coducktor_contract::ApiRun>, coducktor_client::EngineError>,
    },
    RefreshIndex {
        generation: u64,
        result: Result<coducktor_contract::RunsIndexResponse, coducktor_client::EngineError>,
    },
    RefreshProjectRegistry {
        result: Result<coducktor_contract::ProjectsResponse, coducktor_client::EngineError>,
    },
    RefreshModels {
        runner: coducktor_contract::Runner,
        result:
            Result<coducktor_contract::RunnerModelCatalogResponse, coducktor_client::EngineError>,
    },
    RefreshNewTask {
        project: String,
        snapshot: PrimeNewTaskSnapshot,
    },
    LoadSettingsUsage {
        result: Result<coducktor_contract::WorkspaceUsageResponse, coducktor_client::EngineError>,
    },
    LoadSettings {
        project: String,
        snapshot: SettingsSnapshot,
    },
    LoadRepoGit {
        project: String,
        repo: Result<coducktor_contract::RepoResponse, coducktor_client::EngineError>,
    },
    LoadRepoGitChanges {
        project: String,
        changes: Result<coducktor_contract::ChangesPayload, coducktor_client::EngineError>,
    },
    LoadTaskGitChanges {
        project: String,
        id: String,
        run: Result<coducktor_contract::ApiRun, coducktor_client::EngineError>,
        changes: Result<coducktor_contract::ChangesPayload, coducktor_client::EngineError>,
    },
    LoadTaskGitFiles {
        project: String,
        id: String,
        result: Result<coducktor_contract::WorktreeEntry, coducktor_client::EngineError>,
    },
    LoadTaskGitCommits {
        project: String,
        id: String,
        result: Result<coducktor_contract::RunCommitsResponse, coducktor_client::EngineError>,
    },
    LoadTaskGitCommitDiff {
        project: String,
        id: String,
        result: Result<coducktor_contract::RepoCommitPayload, coducktor_client::EngineError>,
    },
    LoadIdeDirectory {
        project: String,
        path: Option<String>,
        result: Result<coducktor_contract::IdeDirectoryResponse, coducktor_client::EngineError>,
    },
    LoadIdeFile {
        project: String,
        path: String,
        result: Result<coducktor_contract::IdeFileResponse, coducktor_client::EngineError>,
    },
    LoadScratchpad {
        project: String,
        result: Result<coducktor_contract::Scratchpad, coducktor_client::EngineError>,
    },
    LoadCompare {
        project: String,
        group_id: String,
        result: Result<coducktor_contract::GroupResponse, coducktor_client::EngineError>,
    },
    LoadCompareVariantDiff {
        project: String,
        group_id: String,
        run_id: String,
        result: Result<coducktor_contract::ChangesPayload, coducktor_client::EngineError>,
    },
    PickVariant {
        project: String,
        result: Result<(), coducktor_client::EngineError>,
    },
    LoadRepoGitCommit {
        project: String,
        result: Result<coducktor_contract::RepoCommitPayload, coducktor_client::EngineError>,
    },
    GithubHandToAgent {
        project: String,
        result: Result<coducktor_contract::CreateRunResponse, coducktor_client::EngineError>,
    },
    GithubPickers {
        project: String,
        workflows: Result<coducktor_contract::WorkflowsResponse, coducktor_client::EngineError>,
        skills: Result<Vec<coducktor_contract::Skill>, coducktor_client::EngineError>,
    },
    LoadSkills {
        project: String,
        result: Result<Vec<coducktor_contract::Skill>, coducktor_client::EngineError>,
    },
    LoadWorkflows {
        project: String,
        result: Result<coducktor_contract::WorkflowsResponse, coducktor_client::EngineError>,
    },
    LoadWorkflowSkills {
        project: String,
        result: Result<Vec<coducktor_contract::Skill>, coducktor_client::EngineError>,
    },
    LoadSettingsConfigFile {
        project: String,
        id: String,
        result: Result<coducktor_contract::AgentConfigFileContent, coducktor_client::EngineError>,
    },
    SessionMutation {
        action: SessionMutation,
        project: String,
        id: String,
        result: Result<(), coducktor_client::EngineError>,
    },
}

#[derive(Clone, Copy)]
enum SessionMutation {
    Send,
    Cancel,
    Continue,
    Finish,
}

type BackgroundJob = Box<dyn FnOnce(tokio::runtime::Handle) + Send>;

/// Run engine futures away from the TUI task on a fixed native-worker pool. In-process engine
/// methods intentionally retain synchronous run/session seams, so a Tokio task alone would only
/// move the freeze to another runtime worker and still leave shutdown waiting on an agent process.
/// The pool is deliberately never joined: a confirmed quit must not wait for a live agent call.
struct BackgroundWorkers {
    sender: BackgroundSender<BackgroundJob>,
    pending: Arc<AtomicUsize>,
    _handles: Vec<thread::JoinHandle<()>>,
}

impl BackgroundWorkers {
    fn new(runtime_handle: tokio::runtime::Handle) -> Self {
        let (sender, receiver) = channel::<BackgroundJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let pending = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(BACKGROUND_WORKER_COUNT);
        for _ in 0..BACKGROUND_WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            let pending = Arc::clone(&pending);
            let runtime_handle = runtime_handle.clone();
            handles.push(thread::spawn(move || {
                loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else {
                        return;
                    };
                    job(runtime_handle.clone());
                    pending.fetch_sub(1, Ordering::Release);
                }
            }));
        }
        Self {
            sender,
            pending,
            _handles: handles,
        }
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn worker_count(&self) -> usize {
        self._handles.len()
    }
}

fn spawn_background<F, T, M>(
    workers: &mut BackgroundWorkers,
    sender: &BackgroundSender<BackgroundResult>,
    future: F,
    map: M,
) where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
    M: FnOnce(T) -> BackgroundResult + Send + 'static,
{
    workers.pending.fetch_add(1, Ordering::Release);
    let sender = sender.clone();
    let job = Box::new(move |handle: tokio::runtime::Handle| {
        let result = handle.block_on(future);
        let _ = sender.send(map(result));
    });
    if workers.sender.send(job).is_err() {
        workers.pending.fetch_sub(1, Ordering::Release);
    }
}

/// Load the data that makes the first screen useful without holding up the first frame. The
/// in-process engine is deliberately retained as the only seam; this task only moves its file
/// and git reads behind the TUI event loop.
fn spawn_prime(engine: Arc<dyn Engine>) -> (JoinHandle<()>, UnboundedReceiver<PrimeSnapshot>) {
    let (sender, receiver) = unbounded_channel();
    let handle = tokio::spawn(async move {
        let health = engine.health().await.ok();
        let project = health
            .as_ref()
            .filter(|health| !health.boot_project.is_empty())
            .map(|health| health.boot_project.clone())
            .unwrap_or_else(|| "main".to_owned());
        let scope = Scope::Project(project);
        let (
            runs,
            projects,
            index,
            workspace_ui_state,
            config,
            skills,
            workflows,
            workspace_config,
            provider_status,
            ui_state,
            repo,
        ) = tokio::join!(
            engine.list_runs(&scope),
            engine.projects(),
            engine.runs_index(),
            engine.workspace_ui_state(),
            engine.config(&scope),
            engine.skills(&scope),
            engine.workflows(&scope),
            engine.workspace_config(),
            engine.provider_status(),
            engine.ui_state(&scope),
            engine.repo(&scope),
        );
        let (repo, branches) = repo.ok().map(repo_snapshot).unwrap_or_default();
        let new_task = PrimeNewTaskSnapshot {
            config: config.ok(),
            skills: skills.ok(),
            workflows: workflows.ok(),
            workspace_config: workspace_config.ok(),
            provider_status: provider_status.ok(),
            ui_state: ui_state.ok(),
            repo,
            branches,
        };
        let _ = sender.send(PrimeSnapshot {
            health,
            runs: runs.ok(),
            projects: projects.ok(),
            index: index.ok(),
            workspace_ui_state: workspace_ui_state.ok(),
            new_task,
        });
    });
    (handle, receiver)
}

fn apply_prime_snapshot(app: &mut App, snapshot: PrimeSnapshot) {
    if let Some(health) = snapshot.health {
        // Adopt the boot project the engine actually knows about — the TUI's "main" default is
        // only a placeholder until the health answer arrives.
        if !health.boot_project.is_empty()
            && app.projects.iter().all(|p| p.id != health.boot_project)
        {
            app.history.navigate(app::Route::Tasks {
                project: health.boot_project.clone(),
            });
            app.default_project = health.boot_project;
        }
        app.set_projects(
            health
                .projects
                .into_iter()
                .map(|project| (project.id, project.name)),
        );
        app.set_provider_states(
            health
                .checks
                .into_iter()
                .map(|check| (backend_check_name(check.name), check.available)),
        );
        app.new_task_ui.data.repo = health.repo;
    }
    if let Some(runs) = snapshot.runs {
        let project = app.current_project().to_owned();
        app.set_tasks_for_project(project, runs);
    }
    if let Some(projects) = snapshot.projects {
        let boot_project = projects.boot_project.clone();
        app.set_projects(
            projects
                .projects
                .iter()
                .map(|project| (project.id.clone(), project.name.clone())),
        );
        app.set_project_registry(projects.projects);
        // Health uses the zero-config `default` sentinel, while the workspace registry can
        // already know the real checkout's project id. Replace that placeholder before the user
        // opens a project-scoped screen, otherwise GitHub/Git reads quite correctly target the
        // launch directory instead of the registered boot project.
        if boot_project != "default"
            && matches!(
                app.route(),
                app::Route::Tasks { project } if project == "default"
            )
        {
            app.default_project = boot_project.clone();
            app.request_navigate(app::Route::Tasks {
                project: boot_project.clone(),
            });
            // Keep the startup interaction model: the first Ctrl+Right should enter Tasks.
            app.focus_sidebar();
            app.queue_pending(PendingAction::RefreshTasks {
                project: boot_project.clone(),
            });
            app.queue_pending(PendingAction::RefreshNewTask {
                project: boot_project,
            });
        }
    }
    if let Some(index) = snapshot.index {
        app.set_global_index(index);
    }
    if let Some(state) = snapshot.workspace_ui_state {
        if let Some(theme) = state
            .appearance
            .as_ref()
            .and_then(|appearance| appearance.theme)
        {
            let name = match theme {
                coducktor_contract::ThemePreference::Dark => crate::theme::ThemeName::Dark,
                coducktor_contract::ThemePreference::Lazyvim => crate::theme::ThemeName::LazyVim,
            };
            app.theme = Theme::new(name, app.theme.capability);
        }
        app.notifications_enabled = state
            .notifications
            .as_ref()
            .and_then(|notifications| notifications.enabled)
            .unwrap_or(false);
        app.settings_ui.workspace_ui_state = Some(state);
    }
    apply_new_task_snapshot(app, snapshot.new_task);
}

fn apply_new_task_snapshot(app: &mut App, snapshot: PrimeNewTaskSnapshot) {
    if let Some(config) = snapshot.config {
        app.new_task_ui.data.config = Some(new_task_form::ComposerConfig::from_config(&config));
    }
    if let Some(skills) = snapshot.skills {
        app.new_task_ui.data.skills = skills;
    }
    if let Some(workflows) = snapshot.workflows {
        app.new_task_ui.data.workflows = workflows.workflows;
    }
    if let Some(workspace_config) = snapshot.workspace_config {
        app.new_task_ui.data.workspace_config = Some(workspace_config);
    }
    if let Some(provider_status) = snapshot.provider_status {
        app.new_task_ui.data.provider_status = Some(provider_status);
    }
    if let Some(ui_state) = snapshot.ui_state {
        app.new_task_ui.data.ui_state = Some(ui_state);
    }
    app.new_task_ui.data.repo = snapshot.repo;
    app.new_task_ui.data.branches = snapshot.branches;
}

fn repo_snapshot(
    response: coducktor_contract::RepoResponse,
) -> (Option<coducktor_contract::RepoInfo>, Vec<String>) {
    match response {
        coducktor_contract::RepoResponse::Present(repo) => (Some(repo.info), repo.branches),
        coducktor_contract::RepoResponse::Empty(_) => (None, Vec::new()),
    }
}

/// Apply `--repo`/`--workflow`/`--model` once the background bootstrap has loaded
/// the project registry. `--repo` switches the active project — re-fetching its
/// tasks and New Task data if it differs from the one `prime_app` already loaded —
/// or leaves a clear notice if the directory isn't a registered project rather than silently
/// staying put. `--workflow`/`--model` preselect the New Task screen.
fn apply_launch_args(app: &mut App, cli: &Cli) {
    if let Some(repo) = &cli.repo {
        match cli::resolve_repo(&app.project_registry, repo) {
            Some(project) => {
                if project != app.default_project {
                    app.default_project = project.clone();
                    app.queue_pending(PendingAction::RefreshTasks {
                        project: project.clone(),
                    });
                    app.queue_pending(PendingAction::RefreshNewTask {
                        project: project.clone(),
                    });
                }
                app.request_navigate(app::Route::Tasks { project });
            }
            None => {
                app.notice = Some(format!(
                    "{} is not a registered project — add it from the TUI's project switcher first",
                    repo.display()
                ));
            }
        }
    }
    if cli.workflow.is_some() || cli.model.is_some() {
        if let Some(workflow) = &cli.workflow {
            if cli::workflow_known(&app.new_task_ui.data.workflows, workflow) {
                app.new_task_ui.draft.source = Some(TaskSource::Workflow {
                    reference: workflow.clone(),
                });
            } else {
                app.notice = Some(format!("workflow {workflow:?} not found for this project"));
            }
        }
        if let Some(model) = &cli.model {
            app.new_task_ui.draft.model = Some(model.clone());
        }
        let project = app.default_project.clone();
        app.request_navigate(app::Route::NewTask { project });
    }
}

/// Run one pending action against the engine and reconcile the app with the
/// engine's answer. Failures surface as a toast rather than a crash.
async fn execute_pending(
    engine: Arc<dyn Engine>,
    app: &mut App,
    background_sender: &BackgroundSender<BackgroundResult>,
    background_handle: &mut BackgroundWorkers,
) {
    for action in app.take_pending_up_to(PENDING_ACTIONS_PER_FRAME) {
        match action {
            PendingAction::Archive {
                project,
                id,
                archived,
            } => {
                let scope = Scope::Project(project.clone());
                match engine.archive_run(&scope, &id, archived).await {
                    Ok(run) => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                        queue_global_index_refresh(app);
                    }
                    Err(error) => app.notice = Some(format!("archive failed: {error}")),
                }
            }
            PendingAction::Delete { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.delete_run(&scope, &id).await {
                    Ok(_) => {
                        app.apply_workspace_event(WorkspaceEvent::RunDeleted { project, id });
                        queue_global_index_refresh(app);
                    }
                    Err(error) => app.notice = Some(format!("delete failed: {error}")),
                }
            }
            PendingAction::Read { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.read_run(&scope, &id).await {
                    Ok(run) => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                        queue_global_index_refresh(app);
                    }
                    Err(error) => app.notice = Some(format!("mark read failed: {error}")),
                }
            }
            PendingAction::Unread { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.unread_run(&scope, &id).await {
                    Ok(run) => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                        queue_global_index_refresh(app);
                    }
                    Err(error) => app.notice = Some(format!("mark unread failed: {error}")),
                }
            }
            PendingAction::RefreshTasks { project } => {
                let scope = Scope::Project(project.clone());
                let generation = app.begin_task_request(&project);
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.list_runs(&scope).await },
                    move |result| BackgroundResult::RefreshTasks {
                        project,
                        generation,
                        result,
                    },
                );
            }
            PendingAction::RefreshIndex => {
                let generation = app.begin_global_index_request();
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.runs_index().await },
                    move |result| BackgroundResult::RefreshIndex { generation, result },
                );
            }
            PendingAction::StartRun { project, input } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.start_run(&scope, input).await },
                    move |result| BackgroundResult::StartRun { project, result },
                );
            }
            PendingAction::ActivateRuns { project } => {
                let scope = Scope::Project(project);
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.activate_runs(&scope).await },
                    |result| BackgroundResult::ActivateRuns { result },
                );
            }
            PendingAction::RefreshNewTask { project } => {
                let project_for_task = project.clone();
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { load_new_task_snapshot(engine_for_task, &project_for_task).await },
                    move |snapshot| BackgroundResult::RefreshNewTask { project, snapshot },
                );
            }
            PendingAction::LoadScratchpad { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.scratchpad(&scope).await },
                    move |result| BackgroundResult::LoadScratchpad { project, result },
                );
            }
            PendingAction::ClearScratchpad { project } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::SetScratchpadInput {
                    content: String::new(),
                };
                match engine.put_scratchpad(&scope, &input).await {
                    Ok(_) if app.scratchpad_ui.project == project => {
                        app.scratchpad_ui.loaded = true;
                        app.scratchpad_ui.saving = false;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        app.scratchpad_ui.saving = false;
                        app.notice = Some(format!("clear scratchpad failed: {error}"));
                    }
                }
            }
            PendingAction::SaveScratchpad { project, content } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::SetScratchpadInput { content };
                match engine.put_scratchpad(&scope, &input).await {
                    Ok(_) if app.scratchpad_ui.project == project => {
                        app.scratchpad_ui.loaded = true;
                        app.scratchpad_ui.saving = false;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        app.scratchpad_ui.saving = false;
                        app.notice = Some(format!("save scratchpad failed: {error}"));
                    }
                }
            }
            PendingAction::RefreshModels { runner } => {
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.models(runner).await },
                    move |result| BackgroundResult::RefreshModels { runner, result },
                );
            }
            PendingAction::PutUiState { project, state } => {
                let scope = Scope::Project(project.clone());
                match engine.put_ui_state(&scope, &state).await {
                    Ok(state) => {
                        app.new_task_ui.data.ui_state = Some(state);
                    }
                    Err(error) => app.notice = Some(format!("ui-state write failed: {error}")),
                }
            }
            PendingAction::SetBaseBranch {
                project,
                base_branch,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::SetConfigInput {
                    base_branch: Some(base_branch),
                    ..coducktor_contract::SetConfigInput::default()
                };
                match engine.put_config(&scope, &input).await {
                    Ok(config) => {
                        app.new_task_ui.data.config =
                            Some(new_task_form::ComposerConfig::from_config(&config));
                    }
                    Err(error) => app.notice = Some(format!("base branch failed: {error}")),
                }
            }
            PendingAction::LoadThread { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        tokio::join!(
                            engine_for_task.get_run(&scope, &id_for_task),
                            engine_for_task.run_history(&scope, &id_for_task, None),
                        )
                    },
                    move |(run, history)| BackgroundResult::LoadThread {
                        project,
                        id,
                        run,
                        history,
                    },
                );
            }
            PendingAction::LoadEarlierThread {
                project,
                id,
                cursor,
            } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .run_history(&scope, &id_for_task, Some(&cursor))
                            .await
                    },
                    move |history| BackgroundResult::LoadEarlierThread {
                        project,
                        id,
                        history,
                    },
                );
            }
            PendingAction::SendMessage { project, id, input } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .send_message(&scope, &id_for_task, input)
                            .await
                            .map(|_| ())
                    },
                    move |result| BackgroundResult::SessionMutation {
                        action: SessionMutation::Send,
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::CancelRun { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .cancel_run(&scope, &id_for_task)
                            .await
                            .map(|_| ())
                    },
                    move |result| BackgroundResult::SessionMutation {
                        action: SessionMutation::Cancel,
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::ContinueRun {
                project,
                id,
                text,
                images,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::ContinueInput {
                    text,
                    images,
                    ..coducktor_contract::ContinueInput::default()
                };
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .continue_run(&scope, &id_for_task, input)
                            .await
                            .map(|_| ())
                    },
                    move |result| BackgroundResult::SessionMutation {
                        action: SessionMutation::Continue,
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::FinishRun { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .finish_run(&scope, &id_for_task)
                            .await
                            .map(|_| ())
                    },
                    move |result| BackgroundResult::SessionMutation {
                        action: SessionMutation::Finish,
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::CreatePr { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.create_pr(&scope, &id_for_task).await },
                    move |result| BackgroundResult::CreatePr {
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::OpenInCli { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.open_in_cli(&scope, &id).await },
                    |result| BackgroundResult::OpenInCli { result },
                );
            }
            PendingAction::RemoveQueuedMessage {
                project,
                id,
                message_id,
            } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.remove_queued_message(&scope, &id, &message_id).await {
                    app.notice = Some(format!("remove message failed: {error}"));
                }
                app.pending.push(PendingAction::LoadThread { project, id });
            }
            PendingAction::CancelAutoResume { project, id } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.cancel_auto_resume(&scope, &id).await {
                    app.notice = Some(format!("cancel auto-resume failed: {error}"));
                }
                app.pending.push(PendingAction::LoadThread { project, id });
            }
            PendingAction::LoadTaskGitChanges { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        tokio::join!(
                            engine_for_task.get_run(&scope, &id_for_task),
                            engine_for_task.run_changes(&scope, &id_for_task),
                        )
                    },
                    move |(run, changes)| BackgroundResult::LoadTaskGitChanges {
                        project,
                        id,
                        run,
                        changes,
                    },
                );
            }
            PendingAction::LoadTaskGitFiles { project, id, path } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .run_files(&scope, &id_for_task, path.as_deref())
                            .await
                    },
                    move |result| BackgroundResult::LoadTaskGitFiles {
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::LoadTaskGitCommits { project, id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.run_commits(&scope, &id_for_task).await },
                    move |result| BackgroundResult::LoadTaskGitCommits {
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::LoadTaskGitCommitDiff { project, id, sha } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                let sha_for_task = sha.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .run_commit(&scope, &id_for_task, &sha_for_task)
                            .await
                    },
                    move |result| BackgroundResult::LoadTaskGitCommitDiff {
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::TaskGitCommit { project, id } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::GitCommitInput {
                    message: app.task_git_ui.commit_message.clone(),
                };
                match engine.git_commit(&scope, &id, input).await {
                    Ok(response) => {
                        app.notice = Some(format!(
                            "committed {}",
                            &response.sha[..response.sha.len().min(7)]
                        ));
                    }
                    Err(error) => app.notice = Some(format!("commit failed: {error}")),
                }
                app.pending
                    .push(PendingAction::LoadTaskGitChanges { project, id });
            }
            PendingAction::TaskGitPush { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.git_push(&scope, &id).await {
                    Ok(response) => {
                        app.notice = Some(if response.upstream_set {
                            format!(
                                "pushed {} to {} (upstream set)",
                                response.branch, response.remote
                            )
                        } else {
                            format!("pushed {} to {}", response.branch, response.remote)
                        });
                    }
                    Err(error) => app.notice = Some(format!("push failed: {error}")),
                }
            }
            PendingAction::LoadRepoGit { project } => {
                let scope = Scope::Project(project.clone());
                let repo_scope = scope.clone();
                let changes_scope = scope;
                let repo_engine = engine.clone();
                let repo_project = project.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { repo_engine.repo(&repo_scope).await },
                    move |repo| BackgroundResult::LoadRepoGit {
                        project: repo_project,
                        repo,
                    },
                );
                let changes_engine = engine.clone();
                let changes_project = project;
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { changes_engine.repo_changes(&changes_scope).await },
                    move |changes| BackgroundResult::LoadRepoGitChanges {
                        project: changes_project,
                        changes,
                    },
                );
            }
            PendingAction::LoadRepoGitCommits { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.repo(&scope).await },
                    move |repo| BackgroundResult::LoadRepoGit { project, repo },
                );
            }
            PendingAction::LoadRepoGitCommitDiff { project, sha } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.repo_commit(&scope, &sha).await },
                    move |result| BackgroundResult::LoadRepoGitCommit { project, result },
                );
            }
            PendingAction::RepoGitBranch {
                project,
                name,
                from,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::RepoBranchRequest { name, from };
                match engine.repo_branch(&scope, &input).await {
                    Ok(response) => {
                        app.notice = Some(format!(
                            "branch {} {}",
                            response.branch,
                            if response.created {
                                "created"
                            } else {
                                "switched"
                            }
                        ));
                    }
                    Err(error) => app.notice = Some(format!("branch failed: {error}")),
                }
                app.pending.push(PendingAction::LoadRepoGit { project });
            }
            PendingAction::LoadCompare { project, group_id } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let group_id_for_task = group_id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.group(&scope, &group_id_for_task).await },
                    move |result| BackgroundResult::LoadCompare {
                        project,
                        group_id,
                        result,
                    },
                );
            }
            PendingAction::LoadCompareVariantDiff {
                project,
                group_id,
                run_id,
            } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let run_id_for_task = run_id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.run_changes(&scope, &run_id_for_task).await },
                    move |result| BackgroundResult::LoadCompareVariantDiff {
                        project,
                        group_id,
                        run_id,
                        result,
                    },
                );
            }
            PendingAction::PickVariant {
                project,
                group_id,
                run_id,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::PickVariantRequest { run_id };
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .pick_variant(&scope, &group_id, &input)
                            .await
                            .map(|_| ())
                    },
                    move |result| BackgroundResult::PickVariant { project, result },
                );
            }
            PendingAction::LoadIdeDirectory { project, path } => {
                let scope = Scope::Project(project.clone());
                // The listed directory IS the screen's current directory — the sidebar entry
                // point queues a root listing, GoUp/Enter queue a subdirectory, and the state
                // must converge on the same path the header renders.
                app.ide_ui.directory_path = path.clone().unwrap_or_default();
                let engine_for_task = engine.clone();
                let path_for_task = path.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .ide_tree(&scope, path_for_task.as_deref())
                            .await
                    },
                    move |result| BackgroundResult::LoadIdeDirectory {
                        project,
                        path,
                        result,
                    },
                );
            }
            PendingAction::LoadIdeFile { project, path } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                let path_for_task = path.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.ide_file(&scope, &path_for_task).await },
                    move |result| BackgroundResult::LoadIdeFile {
                        project,
                        path,
                        result,
                    },
                );
            }
            PendingAction::SaveIdeFile { project, path } => {
                let scope = Scope::Project(project.clone());
                let content = app.ide_ui.editor.text.clone();
                match engine.ide_save(&scope, &path, &content).await {
                    Ok(file) => {
                        app.ide_ui.dirty = false;
                        app.ide_ui.file_size = file.size;
                        app.notice = Some(format!("saved {path}"));
                    }
                    Err(error) => app.notice = Some(format!("save failed: {error}")),
                }
            }
            PendingAction::OpenIdeInEditor { project, path } => {
                // Prefer the registry entry (the root the user added), then the
                // engine's scope resolution, which also knows the workspace root
                // (`default`) and any registered project root.
                let root = app
                    .project_registry
                    .iter()
                    .find(|entry| entry.id == project)
                    .map(|entry| entry.root.clone())
                    .or_else(|| engine.project_root(&Scope::Project(project.clone())).ok());
                match root {
                    Some(root) => {
                        let absolute = if path.is_empty() {
                            root
                        } else {
                            format!("{}/{}", root.trim_end_matches('/'), path)
                        };
                        app.set_editor_handoff(absolute);
                    }
                    None => {
                        app.notice = Some("project root unknown — cannot open in editor".to_owned())
                    }
                }
            }
            PendingAction::IdeDiscardThenNavigate(_) => unreachable!("resolved in app.rs"),
            PendingAction::IdeDiscardThenBack => unreachable!("resolved in app.rs"),
            PendingAction::IdeDiscardThenForward => unreachable!("resolved in app.rs"),
            PendingAction::SwitchProject(_) => unreachable!("resolved in app.rs"),
            PendingAction::LoadGithub { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.github(&scope).await },
                    move |result| BackgroundResult::Github { project, result },
                );
            }
            PendingAction::LoadGithubPickers { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        tokio::join!(
                            engine_for_task.workflows(&scope),
                            engine_for_task.skills(&scope),
                        )
                    },
                    move |(workflows, skills)| BackgroundResult::GithubPickers {
                        project,
                        workflows,
                        skills,
                    },
                );
            }
            PendingAction::LoadGithubComments {
                project,
                kind,
                number,
            } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.github_comments(&scope, &kind, number).await },
                    move |result| BackgroundResult::GithubComments {
                        project,
                        number,
                        result,
                    },
                );
            }
            PendingAction::LoadGithubMergeState { project, number } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.github_pr_merge_state(&scope, number).await },
                    move |result| BackgroundResult::GithubMergeState {
                        project,
                        number,
                        result,
                    },
                );
            }
            PendingAction::LoadGithubPrChanges { project, number } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.github_pr_changes(&scope, number).await },
                    move |result| BackgroundResult::GithubPrChanges {
                        project,
                        number,
                        result,
                    },
                );
            }
            PendingAction::GithubMerge {
                project,
                number,
                method,
                head_sha,
                override_rules,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::GithubMergeInput {
                    method,
                    expected_head_sha: head_sha,
                    override_rules: Some(override_rules),
                };
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .github_merge_pr(&scope, number, &input)
                            .await
                    },
                    move |result| BackgroundResult::GithubMerge {
                        project,
                        number,
                        result,
                    },
                );
            }
            PendingAction::GithubHandToAgent { project, input } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.start_run(&scope, input).await },
                    move |result| BackgroundResult::GithubHandToAgent { project, result },
                );
            }
            PendingAction::LoadSkills { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.skills(&scope).await },
                    move |result| BackgroundResult::LoadSkills { project, result },
                );
            }
            PendingAction::LoadWorkflows { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.workflows(&scope).await },
                    move |result| BackgroundResult::LoadWorkflows { project, result },
                );
            }
            PendingAction::LoadWorkflowSkills { project } => {
                let scope = Scope::Project(project.clone());
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.skills(&scope).await },
                    move |result| BackgroundResult::LoadWorkflowSkills { project, result },
                );
            }
            PendingAction::SaveWorkflow { project } => {
                save_or_export_workflow(engine.as_ref(), app, &project, false).await;
            }
            PendingAction::ExportWorkflow { project } => {
                save_or_export_workflow(engine.as_ref(), app, &project, true).await;
            }
            PendingAction::DeleteWorkflow { project, name } => {
                let scope = Scope::Project(project.clone());
                match engine.delete_workflow(&scope, &name).await {
                    Ok(_) => {
                        app.notice = Some(format!("deleted workflow {name}"));
                        app.pending.push(PendingAction::LoadWorkflows { project });
                    }
                    Err(error) => app.notice = Some(format!("delete failed: {error}")),
                }
            }
            PendingAction::ImportWorkflow { project, yaml } => {
                let scope = Scope::Project(project.clone());
                match engine.parse_workflow(&scope, &yaml).await {
                    Ok(parsed) => {
                        app.workflows_ui.selected_tab = app.workflows_ui.workflows.len();
                        app.workflows_ui.draft_name = parsed.name;
                        app.workflows_ui.draft_steps = parsed.steps;
                        app.notice = Some("imported — review and save".to_owned());
                    }
                    Err(error) => app.notice = Some(format!("import failed: {error}")),
                }
            }
            PendingAction::LoadSettings { project } => {
                let engine_for_task = engine.clone();
                let project_for_task = project.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { engine_for_task.workspace_usage().await },
                    |result| BackgroundResult::LoadSettingsUsage { result },
                );
                let engine_for_task = engine.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move { load_settings_snapshot(engine_for_task, &project_for_task).await },
                    move |snapshot| BackgroundResult::LoadSettings { project, snapshot },
                );
            }
            PendingAction::SettingsPutConfig { project, input } => {
                let scope = Scope::Project(project.clone());
                match engine.put_config(&scope, &input).await {
                    Ok(config) => app.settings_ui.config = Some(config),
                    Err(error) => app.notice = Some(format!("settings: {error}")),
                }
            }
            PendingAction::SettingsPutWorkspaceConfig { input } => {
                match engine.put_workspace_config(&input).await {
                    Ok(config) => app.settings_ui.workspace_config = Some(config),
                    Err(error) => app.notice = Some(format!("settings: {error}")),
                }
            }
            PendingAction::SettingsPutWorkspaceUiState { input } => {
                match engine.put_workspace_ui_state(&input).await {
                    Ok(state) => {
                        app.notifications_enabled = state
                            .notifications
                            .as_ref()
                            .and_then(|notifications| notifications.enabled)
                            .unwrap_or(false);
                        app.settings_ui.workspace_ui_state = Some(state);
                    }
                    Err(error) => app.notice = Some(format!("settings: {error}")),
                }
            }
            PendingAction::SettingsLoadConfigFile { project, id } => {
                let scope = Scope::Project(project.clone());
                app.settings_ui.loading_file = Some(id.clone());
                let engine_for_task = engine.clone();
                let id_for_task = id.clone();
                spawn_background(
                    background_handle,
                    background_sender,
                    async move {
                        engine_for_task
                            .agent_config_file(&scope, &id_for_task)
                            .await
                    },
                    move |result| BackgroundResult::LoadSettingsConfigFile {
                        project,
                        id,
                        result,
                    },
                );
            }
            PendingAction::SettingsPutConfigFile {
                project,
                id,
                content,
                version,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::SetAgentConfigInput { content, version };
                match engine.put_agent_config_file(&scope, &id, &input).await {
                    Ok(file) => {
                        app.settings_ui.file_editor.set_text(&file.content);
                        app.settings_ui.open_file = Some(file);
                        app.settings_ui.file_editing = false;
                        app.pending.push(PendingAction::LoadSettings { project });
                    }
                    Err(error) => app.notice = Some(format!("agent config save failed: {error}")),
                }
            }
            PendingAction::SettingsCreateAgentProfile {
                provider,
                config_dir,
            } => {
                let input = coducktor_contract::CreateAgentProfileInput {
                    provider,
                    label: None,
                    config_dir,
                };
                match engine.create_agent_profile(&input).await {
                    Ok(_) => {
                        app.pending.push(PendingAction::LoadSettings {
                            project: app.settings_ui.project.clone(),
                        });
                    }
                    Err(error) => app.notice = Some(format!("add account failed: {error}")),
                }
            }
            PendingAction::SettingsUpdateAgentProfile { id, input } => {
                match engine.update_agent_profile(&id, &input).await {
                    Ok(_) => {
                        app.pending.push(PendingAction::LoadSettings {
                            project: app.settings_ui.project.clone(),
                        });
                    }
                    Err(error) => app.notice = Some(format!("rename account failed: {error}")),
                }
            }
            PendingAction::SettingsRemoveAgentProfile { id } => {
                match engine.remove_agent_profile(&id).await {
                    Ok(_) => {
                        app.pending.push(PendingAction::LoadSettings {
                            project: app.settings_ui.project.clone(),
                        });
                    }
                    Err(error) => app.notice = Some(format!("remove account failed: {error}")),
                }
            }
            PendingAction::SettingsSelectAgentProfile { input } => {
                match engine.select_agent_profile(&input).await {
                    Ok(_) => {
                        app.pending.push(PendingAction::LoadSettings {
                            project: app.settings_ui.project.clone(),
                        });
                    }
                    Err(error) => app.notice = Some(format!("select account failed: {error}")),
                }
            }
            PendingAction::SettingsRegisterProject { root } => {
                let input = coducktor_contract::RegisterProjectInput { root };
                match engine.register_project(&input).await {
                    Ok(response) => {
                        app.settings_ui.notice = Some(format!(
                            "registered {} — {}",
                            response.project.name, response.project.root
                        ));
                        queue_project_registry_refresh(
                            engine.clone(),
                            background_sender,
                            background_handle,
                        );
                    }
                    Err(error) => {
                        app.settings_ui.notice = Some(format!("add repository failed: {error}"));
                    }
                }
            }
            PendingAction::SettingsReclaimWorktrees { project } => {
                let scope = Scope::Project(project.clone());
                match engine.reclaim_worktrees(&scope).await {
                    Ok(response) => {
                        app.notice = Some(format!(
                            "reclaimed {} worktree(s)",
                            response.reclaimed.len()
                        ));
                        app.pending.push(PendingAction::LoadSettings { project });
                    }
                    Err(error) => app.notice = Some(format!("reclaim failed: {error}")),
                }
            }
            PendingAction::SettingsRemoveWorktree { project, run_id } => {
                let scope = Scope::Project(project.clone());
                match engine.remove_run_worktree(&scope, &run_id).await {
                    Ok(_) => app.pending.push(PendingAction::LoadSettings { project }),
                    Err(error) => app.notice = Some(format!("remove worktree failed: {error}")),
                }
            }
            PendingAction::SettingsRemoveProject { id } => match engine.remove_project(&id).await {
                Ok(_) => {
                    queue_project_registry_refresh(
                        engine.clone(),
                        background_sender,
                        background_handle,
                    );
                }
                Err(error) => app.notice = Some(format!("remove project failed: {error}")),
            },
            PendingAction::SettingsUpdateProject { id, input } => {
                match engine.update_project(&id, &input).await {
                    Ok(_) => {
                        queue_project_registry_refresh(
                            engine.clone(),
                            background_sender,
                            background_handle,
                        );
                    }
                    Err(error) => app.notice = Some(format!("update project failed: {error}")),
                }
            }
            PendingAction::Quit => {}
        }
    }
}

async fn load_new_task_snapshot(engine: Arc<dyn Engine>, project: &str) -> PrimeNewTaskSnapshot {
    let scope = Scope::Project(project.to_owned());
    let (config, skills, workflows, workspace_config, provider_status, ui_state, repo) = tokio::join!(
        engine.config(&scope),
        engine.skills(&scope),
        engine.workflows(&scope),
        engine.workspace_config(),
        engine.provider_status(),
        engine.ui_state(&scope),
        engine.repo(&scope),
    );
    let (repo, branches) = repo.ok().map(repo_snapshot).unwrap_or_default();
    PrimeNewTaskSnapshot {
        config: config.ok(),
        skills: skills.ok(),
        workflows: workflows.ok(),
        workspace_config: workspace_config.ok(),
        provider_status: provider_status.ok(),
        ui_state: ui_state.ok(),
        repo,
        branches,
    }
}

fn apply_started_run(
    app: &mut App,
    project: String,
    result: Result<coducktor_contract::CreateRunResponse, coducktor_client::EngineError>,
    starts_in_flight: &mut HashSet<String>,
) {
    match result {
        Ok(response) => {
            starts_in_flight.remove(&project);
            app.pending_start_drafts.remove(&project);
            app.pending_start_composers.remove(&project);
            let started = match &response {
                coducktor_contract::CreateRunResponse::Single(run) => Some((**run).clone()),
                coducktor_contract::CreateRunResponse::Group { runs } => runs.first().cloned(),
            };
            if let Some(run) = started {
                screens::thread::open_started(app, &project, run);
            }
            app.pending.push(PendingAction::ActivateRuns {
                project: project.clone(),
            });
            screens::new_task::clear_draft(app);
            app.queue_pending(PendingAction::RefreshTasks {
                project: project.clone(),
            });
            if matches!(app.route(), app::Route::GlobalTasks) {
                app.queue_pending(PendingAction::RefreshIndex);
            }
        }
        Err(error) => {
            starts_in_flight.remove(&project);
            screens::new_task::restore_start_draft(app, &project);
            app.notice = Some(format!("start failed: {error}"));
        }
    }
}

fn drain_background_results(
    receiver: &BackgroundReceiver<BackgroundResult>,
    app: &mut App,
    starts_in_flight: &mut HashSet<String>,
) {
    let started = Instant::now();
    for _ in 0..RECEIVER_ITEMS_PER_FRAME {
        if started.elapsed() >= RECEIVER_TIME_BUDGET {
            break;
        }
        let Ok(result) = receiver.try_recv() else {
            break;
        };
        match result {
            BackgroundResult::StartRun { project, result } => {
                apply_started_run(app, project, result, starts_in_flight);
            }
            BackgroundResult::ActivateRuns { result } => {
                if let Err(error) = result {
                    app.notice = Some(format!("start failed: {error}"));
                }
            }
            BackgroundResult::CreatePr {
                project,
                id,
                result,
            } => {
                match result {
                    Ok(response) => {
                        app.notice = Some(format!("draft PR created — {}", response.url));
                    }
                    Err(error) => app.notice = Some(format!("draft PR failed: {error}")),
                }
                app.pending.push(PendingAction::LoadThread { project, id });
            }
            BackgroundResult::OpenInCli { result } => {
                if let Err(error) = result {
                    app.notice = Some(format!("open in terminal failed: {error}"));
                }
            }
            BackgroundResult::Github { project, result } => {
                if app.github_ui.project != project {
                    continue;
                }
                match result {
                    Ok(data) => app.github_ui.data = Some(data),
                    Err(error) => app.notice = Some(format!("load github failed: {error}")),
                }
            }
            BackgroundResult::GithubComments {
                project,
                number,
                result,
            } => {
                if !github_detail_matches(app, &project, number) {
                    continue;
                }
                match result {
                    Ok(comments) => app.github_ui.comments = Some(comments),
                    Err(error) => app.notice = Some(format!("load comments failed: {error}")),
                }
            }
            BackgroundResult::GithubMergeState {
                project,
                number,
                result,
            } => {
                if !github_detail_matches(app, &project, number) {
                    continue;
                }
                match result {
                    Ok(state) => app.github_ui.merge_state = Some(state),
                    Err(error) => app.notice = Some(format!("load merge state failed: {error}")),
                }
            }
            BackgroundResult::GithubPrChanges {
                project,
                number,
                result,
            } => {
                if !github_detail_matches(app, &project, number) {
                    continue;
                }
                match result {
                    Ok(changes) => app.github_ui.pr_changes = Some(changes),
                    Err(error) => app.notice = Some(format!("load changes failed: {error}")),
                }
            }
            BackgroundResult::GithubMerge {
                project,
                number,
                result,
            } => {
                if app.github_ui.project != project {
                    continue;
                }
                match result {
                    Ok(response) => {
                        app.notice = Some(format!("merged PR #{number} with {}", response.method));
                        app.pending.push(PendingAction::LoadGithub { project });
                    }
                    Err(error) => app.notice = Some(format!("merge failed: {error}")),
                }
            }
            BackgroundResult::LoadThread {
                project,
                id,
                run,
                history,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::Thread {
                        project: route_project,
                        id: route_id,
                    } if route_project == &project && route_id == &id
                ) {
                    continue;
                }
                match (run, history) {
                    (Ok(run), Ok(history)) => {
                        let events = history
                            .events
                            .into_iter()
                            .map(thread_history_event)
                            .collect();
                        app.thread_ui.load(
                            project,
                            id,
                            run,
                            events,
                            history.as_of_seq as f64,
                            history.older_cursor,
                        );
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        app.notice = Some(format!("load task failed: {error}"));
                    }
                }
            }
            BackgroundResult::LoadEarlierThread {
                project,
                id,
                history,
            } => {
                if app.thread_ui.data.project != project || app.thread_ui.data.run_id != id {
                    continue;
                }
                match history {
                    Ok(history) => {
                        let events = history
                            .events
                            .into_iter()
                            .map(thread_history_event)
                            .collect();
                        app.thread_ui.merge_earlier(events, history.older_cursor);
                    }
                    Err(error) => app.thread_ui.fail_load_earlier(error.to_string()),
                }
            }
            BackgroundResult::RefreshTasks {
                project,
                generation,
                result,
            } => {
                let error = result.as_ref().err().map(ToString::to_string);
                app.apply_task_response(
                    &project,
                    generation,
                    result.map_err(|error| error.to_string()),
                );
                if let Some(error) = error {
                    app.notice = Some(format!("refresh tasks failed: {error}"));
                }
            }
            BackgroundResult::RefreshIndex { generation, result } => {
                let error = result.as_ref().err().map(ToString::to_string);
                app.apply_global_index_response(
                    generation,
                    result.map_err(|error| error.to_string()),
                );
                if let Some(error) = error {
                    app.notice = Some(format!("refresh all tasks failed: {error}"));
                }
            }
            BackgroundResult::RefreshProjectRegistry { result } => {
                if let Ok(projects) = result {
                    apply_project_registry(app, projects);
                }
            }
            BackgroundResult::RefreshModels { runner, result } => match result {
                Ok(catalog) => {
                    if matches!(app.route(), app::Route::NewTask { .. }) {
                        screens::new_task::apply_model_catalog(app, catalog);
                    } else if matches!(
                        app.route(),
                        app::Route::Settings { .. } | app::Route::GlobalSettings
                    ) {
                        screens::settings::apply_model_catalog(app, catalog);
                    }
                }
                Err(error) => {
                    app.notice = Some(format!("{runner:?} model catalog failed: {error}"))
                }
            },
            BackgroundResult::RefreshNewTask { project, snapshot } => {
                if app.current_project() == project {
                    apply_new_task_snapshot(app, snapshot);
                }
            }
            BackgroundResult::LoadSettingsUsage { result } => match result {
                Ok(usage) => app.settings_ui.workspace_usage = Some(usage),
                Err(error) => app.notice = Some(format!("load provider usage failed: {error}")),
            },
            BackgroundResult::LoadSettings { project, snapshot } => {
                if !matches!(
                    app.route(),
                    app::Route::Settings { project: route_project } if route_project == &project
                ) && !matches!(app.route(), app::Route::GlobalSettings)
                {
                    continue;
                }
                apply_settings_snapshot(app, snapshot);
            }
            BackgroundResult::LoadScratchpad { project, result } => {
                if !matches!(
                    app.route(),
                    app::Route::Scratchpad { project: route_project } if route_project == &project
                ) || app.scratchpad_ui.project != project
                {
                    continue;
                }
                match result {
                    Ok(scratchpad) => {
                        app.scratchpad_ui.editor.set_text(&scratchpad.content);
                        app.scratchpad_ui.loaded = true;
                        app.scratchpad_ui.saving = false;
                    }
                    Err(error) => {
                        app.scratchpad_ui.loaded = true;
                        app.notice = Some(format!("scratchpad: {error}"));
                    }
                }
            }
            BackgroundResult::LoadCompare {
                project,
                group_id,
                result,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::Compare { project: route_project, group_id: route_group_id }
                        if route_project == &project && route_group_id == &group_id
                ) {
                    continue;
                }
                match result {
                    Ok(group) => {
                        let first = group.runs.first().map(|variant| variant.id.clone());
                        app.compare_ui.group = Some(group);
                        if let Some(run_id) = first {
                            app.pending.push(PendingAction::LoadCompareVariantDiff {
                                project,
                                group_id,
                                run_id,
                            });
                        }
                    }
                    Err(error) => app.notice = Some(format!("load compare failed: {error}")),
                }
            }
            BackgroundResult::LoadCompareVariantDiff {
                project,
                group_id,
                run_id,
                result,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::Compare { project: route_project, group_id: route_group_id }
                        if route_project == &project && route_group_id == &group_id
                ) {
                    continue;
                }
                match result {
                    Ok(changes) => {
                        app.compare_ui.variant_diffs.insert(run_id, changes);
                    }
                    Err(error) => app.notice = Some(format!("load diff failed: {error}")),
                }
            }
            BackgroundResult::PickVariant { project, result } => match result {
                Ok(()) => {
                    app.notice = Some("variant picked".to_owned());
                    app.queue_pending(PendingAction::RefreshTasks { project });
                }
                Err(error) => app.notice = Some(format!("pick failed: {error}")),
            },
            BackgroundResult::LoadRepoGit { project, repo } => {
                if app.repo_git_ui.project != project {
                    continue;
                }
                match repo {
                    Ok(repo) => app.repo_git_ui.repo = Some(repo),
                    Err(error) => app.notice = Some(format!("load repo failed: {error}")),
                }
            }
            BackgroundResult::LoadRepoGitChanges { project, changes } => {
                if app.repo_git_ui.project != project {
                    continue;
                }
                app.repo_git_ui.changes_loading = false;
                if let Ok(changes) = changes {
                    app.repo_git_ui.repo_changes_files = changes.files;
                }
            }
            BackgroundResult::LoadTaskGitChanges {
                project,
                id,
                run,
                changes,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::TaskGit { project: route_project, id: route_id, .. }
                        if route_project == &project && route_id == &id
                ) {
                    continue;
                }
                match (run, changes) {
                    (Ok(run), Ok(changes)) => {
                        app.task_git_ui.run = Some(run);
                        app.task_git_ui.changes = Some(changes);
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        app.notice = Some(format!("load changes failed: {error}"));
                    }
                }
            }
            BackgroundResult::LoadTaskGitFiles {
                project,
                id,
                result,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::TaskGit { project: route_project, id: route_id, tab: app::TaskGitTab::Files }
                        if route_project == &project && route_id == &id
                ) {
                    continue;
                }
                match result {
                    Ok(entry) => app.task_git_ui.files_entry = Some(entry),
                    Err(error) => app.notice = Some(format!("load files failed: {error}")),
                }
            }
            BackgroundResult::LoadTaskGitCommits {
                project,
                id,
                result,
            } => {
                if !matches!(app.route(), app::Route::TaskGit { project: route_project, id: route_id, tab: app::TaskGitTab::Commits } if route_project == &project && route_id == &id)
                {
                    continue;
                }
                match result {
                    Ok(commits) => app.task_git_ui.commits = Some(commits),
                    Err(error) => app.notice = Some(format!("load commits failed: {error}")),
                }
            }
            BackgroundResult::LoadTaskGitCommitDiff {
                project,
                id,
                result,
            } => {
                if !matches!(app.route(), app::Route::TaskGit { project: route_project, id: route_id, tab: app::TaskGitTab::Commits } if route_project == &project && route_id == &id)
                {
                    continue;
                }
                match result {
                    Ok(commit) => app.task_git_ui.commit_detail = Some(commit),
                    Err(error) => app.notice = Some(format!("load commit failed: {error}")),
                }
            }
            BackgroundResult::LoadIdeDirectory {
                project,
                path,
                result,
            } => {
                if !matches!(app.route(), app::Route::Ide { project: route_project } if route_project == &project)
                    || app.ide_ui.directory_path != path.unwrap_or_default()
                {
                    continue;
                }
                match result {
                    Ok(directory) => {
                        app.ide_ui.entries = Some(directory);
                        app.ide_ui.tree_selected = 0;
                    }
                    Err(error) => app.notice = Some(format!("load directory failed: {error}")),
                }
            }
            BackgroundResult::LoadIdeFile {
                project,
                path,
                result,
            } => {
                if !matches!(app.route(), app::Route::Ide { project: route_project } if route_project == &project)
                    || app.ide_ui.file_path.as_deref() != Some(path.as_str())
                {
                    continue;
                }
                match result {
                    Ok(file) => {
                        // The draft survives a reload only when it is still pristine — a
                        // reload while dirty would silently eat the user's edits.
                        if app.ide_ui.dirty {
                            app.notice = Some("unsaved changes kept — reload skipped".to_owned());
                        } else {
                            app.ide_ui.editor.set_text(&file.content);
                            app.ide_ui.file_size = file.size;
                            app.ide_ui.file_error = None;
                        }
                    }
                    Err(error) => app.ide_ui.file_error = Some(error.to_string()),
                }
            }
            BackgroundResult::LoadRepoGitCommit { project, result } => {
                if app.repo_git_ui.project != project {
                    continue;
                }
                match result {
                    Ok(commit) => app.repo_git_ui.commit_detail = Some(commit),
                    Err(error) => app.notice = Some(format!("load commit failed: {error}")),
                }
            }
            BackgroundResult::GithubHandToAgent { project, result } => {
                if app.github_ui.project != project {
                    continue;
                }
                match result {
                    Ok(response) => {
                        if let Some(id) = new_task_form::started_run_id(&response) {
                            app.github_ui.queued = Some(id);
                        }
                        app.pending.push(PendingAction::ActivateRuns {
                            project: project.clone(),
                        });
                        app.queue_pending(PendingAction::RefreshTasks { project });
                    }
                    Err(error) => app.notice = Some(format!("start failed: {error}")),
                }
            }
            BackgroundResult::GithubPickers {
                project,
                workflows,
                skills,
            } => {
                if !matches!(app.route(), app::Route::Github { project: route_project } if route_project == &project)
                    || app.github_ui.project != project
                {
                    continue;
                }
                if let Ok(workflows) = workflows {
                    app.github_ui.workflows = workflows.workflows;
                }
                if let Ok(skills) = skills {
                    app.github_ui.skills = skills;
                }
            }
            BackgroundResult::LoadSkills { project, result } => {
                if !matches!(app.route(), app::Route::Skills { project: route_project } if route_project == &project)
                    || app.skills_ui.project != project
                {
                    continue;
                }
                match result {
                    Ok(skills) => app.skills_ui.skills = skills,
                    Err(error) => app.notice = Some(format!("load skills failed: {error}")),
                }
            }
            BackgroundResult::LoadWorkflows { project, result } => {
                if !matches!(app.route(), app::Route::Workflows { project: route_project } if route_project == &project)
                    || app.workflows_ui.project != project
                {
                    continue;
                }
                match result {
                    Ok(workflows) => {
                        app.workflows_ui.workflows = workflows.workflows;
                        if let Some(issue) = workflows.issues.first() {
                            app.notice = Some(format!(
                                "workflow issue: {} — {}",
                                issue.path, issue.message
                            ));
                        }
                    }
                    Err(error) => app.notice = Some(format!("load workflows failed: {error}")),
                }
            }
            BackgroundResult::LoadWorkflowSkills { project, result } => {
                if !matches!(app.route(), app::Route::Workflows { project: route_project } if route_project == &project)
                    || app.workflows_ui.project != project
                {
                    continue;
                }
                match result {
                    Ok(skills) => app.workflows_ui.palette_skills = skills,
                    Err(error) => app.notice = Some(format!("load skills failed: {error}")),
                }
            }
            BackgroundResult::LoadSettingsConfigFile {
                project,
                id,
                result,
            } => {
                if !matches!(
                    app.route(),
                    app::Route::Settings { project: route_project } if route_project == &project
                ) || app.settings_ui.project != project
                    || app.settings_ui.loading_file.as_deref() != Some(id.as_str())
                {
                    continue;
                }
                app.settings_ui.loading_file = None;
                match result {
                    Ok(file) => {
                        app.settings_ui.file_editor.set_text(&file.content);
                        app.settings_ui.open_file = Some(file);
                        app.settings_ui.file_editing = true;
                    }
                    Err(error) => app.notice = Some(format!("agent config: {error}")),
                }
            }
            BackgroundResult::SessionMutation {
                action,
                project,
                id,
                result,
            } => {
                let label = match action {
                    SessionMutation::Send => "send",
                    SessionMutation::Cancel => "cancel",
                    SessionMutation::Continue => "continue",
                    SessionMutation::Finish => "finish",
                };
                if let Err(error) = result {
                    if matches!(action, SessionMutation::Cancel)
                        && app.thread_ui.data.project == project
                        && app.thread_ui.data.run_id == id
                    {
                        app.thread_ui.cancel_pending = false;
                    }
                    if matches!(action, SessionMutation::Send | SessionMutation::Continue) {
                        app.thread_ui.restore_pending_prompt(&project, &id);
                    }
                    app.notice = Some(format!("{label} failed: {error}"));
                }
                app.pending.push(PendingAction::LoadThread { project, id });
            }
        }
    }
}

fn github_detail_matches(app: &App, project: &str, number: u64) -> bool {
    app.github_ui.project == project
        && app
            .github_ui
            .detail_item
            .as_ref()
            .is_some_and(|item| item.number == number)
}

fn queue_project_registry_refresh(
    engine: Arc<dyn Engine>,
    sender: &BackgroundSender<BackgroundResult>,
    workers: &mut BackgroundWorkers,
) {
    spawn_background(
        workers,
        sender,
        async move { engine.projects().await },
        |result| BackgroundResult::RefreshProjectRegistry { result },
    );
}

fn apply_project_registry(app: &mut App, projects: coducktor_contract::ProjectsResponse) {
    app.set_projects(
        projects
            .projects
            .iter()
            .map(|project| (project.id.clone(), project.name.clone())),
    );
    app.set_project_registry(projects.projects);
}

/// Every Settings data source, in one place — the section list needs all of it at once
/// rather than per-section lazy loads, since Tab cycling between sections must not each
/// re-trigger a fetch.
async fn load_settings_snapshot(engine: Arc<dyn Engine>, project: &str) -> SettingsSnapshot {
    let scope = Scope::Project(project.to_owned());
    let (
        config,
        workspace_config,
        workspace_ui_state,
        ui_state,
        agent_config,
        agent_profiles,
        worktrees,
    ) = tokio::join!(
        engine.config(&scope),
        engine.workspace_config(),
        engine.workspace_ui_state(),
        engine.ui_state(&scope),
        engine.agent_config(&scope),
        engine.agent_profiles(),
        engine.worktrees(&scope),
    );
    SettingsSnapshot {
        config: config.ok(),
        workspace_config: workspace_config.ok(),
        workspace_ui_state: workspace_ui_state.ok(),
        ui_state: ui_state.ok(),
        agent_config: agent_config.ok(),
        agent_profiles: agent_profiles.ok(),
        worktrees: worktrees.ok(),
    }
}

fn apply_settings_snapshot(app: &mut App, snapshot: SettingsSnapshot) {
    if let Some(config) = snapshot.config {
        app.settings_ui.config = Some(config);
    }
    if let Some(config) = snapshot.workspace_config {
        app.settings_ui.workspace_config = Some(config);
    }
    if let Some(state) = snapshot.workspace_ui_state {
        app.notifications_enabled = state
            .notifications
            .as_ref()
            .and_then(|notifications| notifications.enabled)
            .unwrap_or(false);
        app.settings_ui.workspace_ui_state = Some(state);
    }
    if let Some(state) = snapshot.ui_state {
        app.settings_ui.ui_state = Some(state);
    }
    if let Some(listing) = snapshot.agent_config {
        app.settings_ui.agent_config = Some(listing);
    }
    if let Some(profiles) = snapshot.agent_profiles {
        app.settings_ui.agent_profiles = Some(profiles);
    }
    if let Some(worktrees) = snapshot.worktrees {
        app.settings_ui.worktrees = Some(worktrees);
    }
}

fn thread_history_event(
    event: coducktor_contract::RunHistoryEvent,
) -> coducktor_contract::RunEvent {
    coducktor_contract::RunEvent {
        seq: event.seq,
        ts: event.ts,
        step_id: event.step_id,
        event_type: event.event_type,
        extra: event.extra,
    }
}

fn queue_global_index_refresh(app: &mut App) {
    if matches!(app.route(), app::Route::GlobalTasks) {
        app.queue_pending(PendingAction::RefreshIndex);
    }
}

async fn open_workspace_listener(
    engine: Arc<dyn Engine>,
    _project: String,
) -> Option<(JoinHandle<()>, UnboundedReceiver<WorkspaceEvent>)> {
    let (sender, receiver) = unbounded_channel();
    let handle = tokio::spawn(async move {
        let mut events = engine.subscribe(Topic::Named("workspace".to_owned()));
        while let Some(event) = events.next().await {
            // Workspace notifications do not carry a project id. Resolve them at drain time so
            // the listener remains valid when bootstrap or the project switcher changes the
            // active project after this task was spawned.
            if let Some(event) = parse_workspace_event(event, "")
                && sender.send(event).is_err()
            {
                return;
            }
        }
    });
    Some((handle, receiver))
}

fn backend_check_name(name: BackendCheckName) -> String {
    match name {
        BackendCheckName::Claude => "claude".to_owned(),
        BackendCheckName::Codex => "codex".to_owned(),
        BackendCheckName::OpenCode => "opencode".to_owned(),
        BackendCheckName::Pi => "pi".to_owned(),
        BackendCheckName::Gh => "gh".to_owned(),
        BackendCheckName::Git => "git".to_owned(),
    }
}

/// The currently open thread's live event stream — opened when the route enters
/// `Route::Thread`, aborted the moment it leaves. Unlike the workspace listener (one for the
/// whole session), this one is per-navigation.
struct ThreadListener {
    project: String,
    id: String,
    handle: JoinHandle<()>,
    receiver: UnboundedReceiver<EngineEvent>,
    pending_events: Vec<coducktor_contract::RunEvent>,
}

async fn open_run_listener(engine: Arc<dyn Engine>, project: String, id: String) -> ThreadListener {
    let (sender, receiver) = unbounded_channel();
    let handle = tokio::spawn({
        let project_for_topic = project.clone();
        let id = id.clone();
        async move {
            let mut stream = engine.subscribe(Topic::Run {
                project: project_for_topic,
                id,
            });
            while let Some(event) = stream.next().await {
                if sender.send(event).is_err() {
                    return;
                }
            }
        }
    });
    ThreadListener {
        project,
        id,
        handle,
        receiver,
        pending_events: Vec::new(),
    }
}

async fn run(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine: Arc<dyn Engine>,
    workspace_events: Option<&mut UnboundedReceiver<WorkspaceEvent>>,
    cli: &Cli,
) -> io::Result<()> {
    let mut workspace_events = workspace_events;
    let mut thread_listener: Option<ThreadListener> = None;
    let mut bootstrap: Option<(JoinHandle<()>, UnboundedReceiver<PrimeSnapshot>)> = None;
    let (background_sender, background_receiver) = channel();
    let mut background_handle = BackgroundWorkers::new(tokio::runtime::Handle::current());
    let mut starts_in_flight = HashSet::new();
    let mut welcome = WelcomeAnimation::new();
    let mut last_needs_you = usize::MAX;
    let mut bootstrap_applied = false;
    let mut launch_args_applied =
        cli.repo.is_none() && cli.workflow.is_none() && cli.model.is_none();
    while !app.should_quit() {
        let frame_started = Instant::now();
        app.now_epoch = current_epoch_seconds();
        app.animation_tick = app.animation_tick.wrapping_add(1);
        if let Some((_, receiver)) = bootstrap.as_mut()
            && !bootstrap_applied
        {
            match receiver.try_recv() {
                Ok(snapshot) => {
                    apply_prime_snapshot(app, snapshot);
                    bootstrap_applied = true;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    bootstrap_applied = true;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            }
        }
        if bootstrap_applied && !launch_args_applied {
            apply_launch_args(app, cli);
            launch_args_applied = true;
        }
        let mut pending_mouse = None;
        let welcome_was_active = welcome.is_active();
        for _ in 0..INPUT_ITEMS_PER_FRAME {
            if !event::poll(Duration::ZERO)? {
                break;
            }
            match event::read()? {
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => {
                    pending_mouse = Some(Event::Mouse(mouse));
                }
                event if welcome_was_active => {
                    if welcome.handle_event(&event) {
                        app.handle_event(event);
                    }
                }
                event => app.handle_event(event),
            }
        }
        if let Some(mouse) = pending_mouse
            && !welcome_was_active
        {
            app.handle_event(mouse);
        }
        for action in &app.pending {
            if let PendingAction::StartRun { project, .. } = action {
                starts_in_flight.insert(project.clone());
            }
        }
        if let Some(events) = workspace_events.as_deref_mut() {
            let started = Instant::now();
            for _ in 0..RECEIVER_ITEMS_PER_FRAME {
                if started.elapsed() >= RECEIVER_TIME_BUDGET {
                    break;
                }
                let Ok(event) = events.try_recv() else {
                    break;
                };
                match event {
                    WorkspaceEvent::Run { project, run } => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                    }
                    other => app.apply_workspace_event(other),
                }
            }
        }
        drain_background_results(&background_receiver, app, &mut starts_in_flight);
        let desired_thread = match app.route() {
            app::Route::Thread { project, id } => Some((project.clone(), id.clone())),
            _ => None,
        };
        let listener_matches = thread_listener
            .as_ref()
            .map(|listener| (listener.project.clone(), listener.id.clone()))
            == desired_thread;
        if !listener_matches {
            if let Some(listener) = thread_listener.take() {
                listener.handle.abort();
            }
            if let Some((project, id)) = desired_thread {
                thread_listener = Some(open_run_listener(engine.clone(), project, id).await);
            }
        }
        if let Some(listener) = thread_listener.as_mut() {
            let mut live_batch = Vec::new();
            let started = Instant::now();
            for _ in 0..RECEIVER_ITEMS_PER_FRAME {
                if started.elapsed() >= RECEIVER_TIME_BUDGET {
                    break;
                }
                let Ok(event) = listener.receiver.try_recv() else {
                    break;
                };
                if event.data.get("type").and_then(serde_json::Value::as_str) != Some("run-event") {
                    continue;
                }
                let Some(run_event) = event.data.get("event").cloned().and_then(|event| {
                    serde_json::from_value::<coducktor_contract::RunEvent>(event).ok()
                }) else {
                    continue;
                };
                if app.thread_ui.data.project == listener.project
                    && app.thread_ui.data.run_id == listener.id
                {
                    live_batch.push((run_event.seq, run_event));
                } else if matches!(
                    app.route(),
                    app::Route::Thread { project, id }
                        if project == &listener.project && id == &listener.id
                ) {
                    // The durable history read can race the first live events. Keep them until
                    // `ThreadUi::load` establishes its sequence watermark, then fold them below.
                    listener.pending_events.push(run_event);
                }
            }
            app.thread_ui.push_events(live_batch);
        }
        // Bracketed paste is enabled for the whole TUI so composers receive multiline clipboard
        // contents as one event, while the embedded Terminal tab forwards that same event to its
        // shell.
        screens::terminal::maintain(app);
        if let Some(listener) = thread_listener.as_mut()
            && app.thread_ui.data.project == listener.project
            && app.thread_ui.data.run_id == listener.id
        {
            let pending = std::mem::take(&mut listener.pending_events)
                .into_iter()
                .map(|event| (event.seq, event));
            app.thread_ui.push_events(pending);
        }
        if !app.pending.is_empty() {
            execute_pending(
                engine.clone(),
                app,
                &background_sender,
                &mut background_handle,
            )
            .await;
        }
        for (summary, body) in app.take_pending_notifications() {
            crate::notify::notify(app.notifications_enabled, &summary, &body);
            crate::notify::play_sound(app.notifications_enabled);
        }
        let needs_you = app.needs_you_count();
        if needs_you != last_needs_you {
            crate::notify::set_title(&crate::notify::title_for(needs_you));
            last_needs_you = needs_you;
        }
        // The IDE's `Ctrl+E` escape hatch: main owns the terminal, so the
        // suspend → $EDITOR → resume dance lives here, not in the screen or the engine.
        if let Some(path) = app.take_editor_handoff() {
            run_editor_handoff(terminal, &path)?;
            // Whatever the editor wrote to disk wins; reload it over the TUI draft.
            if let Some(file_path) = app.ide_ui.file_path.clone() {
                let project = app.ide_ui.project.clone();
                app.ide_ui.dirty = false;
                app.notice = Some(format!("reloaded {file_path} after $EDITOR"));
                app.pending.push(PendingAction::LoadIdeFile {
                    project,
                    path: file_path,
                });
            }
        }
        terminal.draw(|frame| {
            if welcome.is_active() {
                welcome.render(frame, &app.theme);
            } else {
                app.render(frame);
            }
        })?;
        if bootstrap.is_none() && !app.should_quit() {
            bootstrap = Some(spawn_prime(engine.clone()));
        }

        let remaining = FRAME_BUDGET.saturating_sub(frame_started.elapsed());
        if !remaining.is_zero() {
            let _ = event::poll(remaining)?;
        }
    }
    if let Some(listener) = thread_listener {
        listener.handle.abort();
    }
    if let Some((handle, _)) = bootstrap {
        handle.abort();
    }

    Ok(())
}

/// Save or export the workflows draft. The export path returns the file path it wrote. The body
/// uses the compact `skills:` form when every step is a plain skill step and `steps:` otherwise.
async fn save_or_export_workflow(engine: &dyn Engine, app: &mut App, project: &str, export: bool) {
    let name = if app.workflows_ui.draft_name.trim().is_empty() {
        app.workflows_ui
            .workflows
            .get(app.workflows_ui.selected_tab)
            .map(|workflow| workflow.name.clone())
            .unwrap_or_else(|| "my-chain".to_owned())
    } else {
        app.workflows_ui.draft_name.trim().to_owned()
    };
    let steps = if app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len() {
        app.workflows_ui.draft_steps.clone()
    } else {
        app.workflows_ui.workflows[app.workflows_ui.selected_tab]
            .steps
            .clone()
    };
    if steps.is_empty() {
        app.notice = Some("nothing to save — add steps first".to_owned());
        return;
    }
    // The compact form is exclusive with the full form: provide either steps or skills, not both.
    let input = match screens::workflows::skill_stack_of(&steps) {
        Some(skills) => coducktor_contract::SaveWorkflowInput {
            name,
            description: None,
            steps: None,
            skills: Some(skills),
            overwrite: Some(true),
        },
        None => coducktor_contract::SaveWorkflowInput {
            name,
            description: None,
            steps: Some(steps),
            skills: None,
            overwrite: Some(true),
        },
    };
    let scope = Scope::Project(project.to_owned());
    match engine.save_workflow(&scope, &input).await {
        Ok(response) => {
            app.notice = Some(if export {
                format!("exported {} → {}", response.name, response.path)
            } else {
                format!("saved {} → {}", response.name, response.path)
            });
            app.pending.push(PendingAction::LoadWorkflows {
                project: project.to_owned(),
            });
        }
        Err(error) => app.notice = Some(format!("save failed: {error}")),
    }
}

fn current_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Suspend the TUI (raw mode + alternate screen off), run `$VISUAL`/`$EDITOR`/`vi` on the
/// file in the real terminal, then re-enter raw mode and the alternate screen. The editor is
/// the only foreground handoff; Coducktor has no service child whose output can leak into the
/// terminal.
fn run_editor_handoff(terminal: &mut AppTerminal, path: &str) -> io::Result<()> {
    use crossterm::cursor;
    use crossterm::event::EnableMouseCapture;
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};

    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());
    let (program, arguments) = parse_editor_command(&editor)?;

    terminal.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;

    let result = std::process::Command::new(&program)
        .args(&arguments)
        .arg(path)
        .status();

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    stdout.flush()?;
    terminal.clear()?;

    result
        .map(|_| ())
        .map_err(|error| io::Error::other(format!("failed to run {program}: {error}")))
}

fn parse_editor_command(raw: &str) -> io::Result<(String, Vec<String>)> {
    if raw.chars().count() > 4_096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "editor command is too long",
        ));
    }

    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in raw.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some(delimiter) if character == delimiter => quote = None,
            Some('"') if character == '\\' => escaped = true,
            Some(_) => word.push(character),
            None if character == '\\' => {
                escaped = true;
                started = true;
            }
            None if character == '\'' || character == '"' => {
                quote = Some(character);
                started = true;
            }
            None if character.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(character);
                started = true;
            }
        }
    }
    if escaped || quote.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unterminated escape or quote in editor command",
        ));
    }
    if started {
        words.push(word);
    }
    let mut words = words.into_iter();
    let Some(program) = words.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "editor command is empty",
        ));
    };
    Ok((program, words.collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_index_refreshes_are_queued_and_coalesced_after_mutations() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate_route(app::Route::GlobalTasks);

        queue_global_index_refresh(&mut app);
        queue_global_index_refresh(&mut app);

        assert_eq!(app.pending, vec![PendingAction::RefreshIndex]);
    }

    #[test]
    fn launch_repo_switch_queues_background_refreshes() {
        let repo = tempfile::tempdir().unwrap();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_project_registry(vec![coducktor_contract::ProjectListEntry {
            id: "other".to_owned(),
            root: repo.path().display().to_string(),
            ..coducktor_contract::ProjectListEntry::default()
        }]);
        let cli = Cli {
            command: None,
            repo: Some(repo.path().to_owned()),
            workflow: None,
            model: None,
        };

        apply_launch_args(&mut app, &cli);

        assert_eq!(app.default_project, "other");
        assert!(matches!(app.route(), app::Route::Tasks { project } if project == "other"));
        assert_eq!(
            app.pending,
            vec![
                PendingAction::RefreshTasks {
                    project: "other".to_owned()
                },
                PendingAction::RefreshNewTask {
                    project: "other".to_owned()
                },
            ]
        );
    }

    #[test]
    fn stale_ide_loads_do_not_replace_the_current_directory_or_file() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate_route(app::Route::Ide {
            project: "main".to_owned(),
        });
        app.ide_ui.directory_path = "src".to_owned();
        app.ide_ui.file_path = Some("src/current.rs".to_owned());
        app.ide_ui.editor.set_text("current");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadIdeDirectory {
                project: "main".to_owned(),
                path: Some("old".to_owned()),
                result: Ok(coducktor_contract::IdeDirectoryResponse {
                    path: "old".to_owned(),
                    entries: Vec::new(),
                    truncated: false,
                }),
            })
            .unwrap();
        sender
            .send(BackgroundResult::LoadIdeFile {
                project: "main".to_owned(),
                path: "src/old.rs".to_owned(),
                result: Ok(coducktor_contract::IdeFileResponse {
                    path: "src/old.rs".to_owned(),
                    content: "stale".to_owned(),
                    size: 5,
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.ide_ui.entries.is_none());
        assert_eq!(app.ide_ui.editor.text, "current");
    }

    #[test]
    fn stale_scratchpad_load_does_not_hydrate_another_project() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::scratchpad::open(&mut app, "main");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadScratchpad {
                project: "other".to_owned(),
                result: Ok(coducktor_contract::Scratchpad {
                    content: "stale notes".to_owned(),
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(!app.scratchpad_ui.loaded);
        assert!(app.scratchpad_ui.editor.text.is_empty());
    }

    #[test]
    fn stale_compare_loads_do_not_replace_the_active_variant_group() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::compare::open(&mut app, "main", "current-group");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadCompare {
                project: "main".to_owned(),
                group_id: "old-group".to_owned(),
                result: Ok(coducktor_contract::GroupResponse {
                    group_id: "old-group".to_owned(),
                    runs: Vec::new(),
                }),
            })
            .unwrap();
        sender
            .send(BackgroundResult::LoadCompareVariantDiff {
                project: "main".to_owned(),
                group_id: "old-group".to_owned(),
                run_id: "old-run".to_owned(),
                result: Ok(coducktor_contract::ChangesPayload {
                    files: Vec::new(),
                    stat: coducktor_contract::RepoDiffStat {
                        adds: 0.0,
                        dels: 0.0,
                        files: 0.0,
                    },
                    repointed_head: None,
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.compare_ui.group.is_none());
        assert!(app.compare_ui.variant_diffs.is_empty());
    }

    #[test]
    fn picked_variant_queues_a_background_task_refresh() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::PickVariant {
                project: "main".to_owned(),
                result: Ok(()),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert_eq!(app.notice.as_deref(), Some("variant picked"));
        assert_eq!(
            app.pending,
            vec![PendingAction::RefreshTasks {
                project: "main".to_owned()
            }]
        );
    }

    #[test]
    fn created_pr_queues_a_thread_refresh_after_completion() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::CreatePr {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
                result: Ok(coducktor_contract::CreatePrResponse {
                    url: "https://example.test/pr/1".to_owned(),
                    dry_run: false,
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert_eq!(
            app.notice.as_deref(),
            Some("draft PR created — https://example.test/pr/1")
        );
        assert_eq!(
            app.pending,
            vec![PendingAction::LoadThread {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
            }]
        );
    }

    #[test]
    fn failed_cli_handoff_reports_after_background_completion() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::OpenInCli {
                result: Err(coducktor_client::EngineError::Conflict {
                    reason: "no terminal launcher".to_owned(),
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert_eq!(
            app.notice.as_deref(),
            Some("open in terminal failed: conflict: no terminal launcher")
        );
    }

    #[test]
    fn failed_run_activation_reports_after_background_completion() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::ActivateRuns {
                result: Err(coducktor_client::EngineError::Unavailable {
                    reason: "runner worker unavailable".to_owned(),
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert_eq!(
            app.notice.as_deref(),
            Some("start failed: service unavailable: runner worker unavailable")
        );
    }

    #[test]
    fn stale_github_picker_load_does_not_update_the_active_project() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::github::open(&mut app, "main");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::GithubPickers {
                project: "other".to_owned(),
                workflows: Ok(coducktor_contract::WorkflowsResponse {
                    workflows: Vec::new(),
                    issues: Vec::new(),
                }),
                skills: Ok(Vec::new()),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.github_ui.workflows.is_empty());
        assert!(app.github_ui.skills.is_empty());
    }

    #[test]
    fn stale_workflow_loads_do_not_update_the_active_project() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::workflows::open(&mut app, "main");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadWorkflows {
                project: "other".to_owned(),
                result: Ok(coducktor_contract::WorkflowsResponse {
                    workflows: Vec::new(),
                    issues: Vec::new(),
                }),
            })
            .unwrap();
        sender
            .send(BackgroundResult::LoadWorkflowSkills {
                project: "other".to_owned(),
                result: Ok(Vec::new()),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.workflows_ui.workflows.is_empty());
        assert!(app.workflows_ui.palette_skills.is_empty());
    }

    #[test]
    fn stale_skill_load_does_not_update_the_active_project() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::skills::open(&mut app, "main");
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadSkills {
                project: "other".to_owned(),
                result: Ok(Vec::new()),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.skills_ui.skills.is_empty());
    }

    #[test]
    fn stale_agent_config_load_does_not_open_the_wrong_file() {
        let (sender, receiver) = channel();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        screens::settings::open(&mut app, "main");
        app.settings_ui.loading_file = Some("current".to_owned());
        let mut starts_in_flight = HashSet::new();

        sender
            .send(BackgroundResult::LoadSettingsConfigFile {
                project: "main".to_owned(),
                id: "old".to_owned(),
                result: Ok(coducktor_contract::AgentConfigFileContent {
                    id: "old".to_owned(),
                    path: ".agent/old.json".to_owned(),
                    exists: true,
                    content: "stale".to_owned(),
                    version: Some("1".to_owned()),
                }),
            })
            .unwrap();

        drain_background_results(&receiver, &mut app, &mut starts_in_flight);

        assert!(app.settings_ui.open_file.is_none());
        assert_eq!(app.settings_ui.loading_file.as_deref(), Some("current"));
    }

    #[tokio::test]
    async fn background_work_uses_a_fixed_worker_pool() {
        let (sender, receiver) = channel();
        let mut workers = BackgroundWorkers::new(tokio::runtime::Handle::current());
        for _ in 0..1_000 {
            spawn_background(&mut workers, &sender, async {}, |_| {
                BackgroundResult::LoadSettingsUsage {
                    result: Err(coducktor_client::EngineError::Unavailable {
                        reason: "test worker".to_owned(),
                    }),
                }
            });
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            let mut completed = 0;
            loop {
                while receiver.try_recv().is_ok() {
                    completed += 1;
                }
                if completed == 1_000 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if workers.pending_count() == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(workers.worker_count(), BACKGROUND_WORKER_COUNT);
        assert_eq!(workers.pending_count(), 0);
    }

    #[test]
    fn in_process_workspace_events_decode_shell_badges() {
        let run = parse_workspace_event(
            EngineEvent {
                topic: "workspace".to_owned(),
                data: serde_json::json!({
                    "type": "run",
                    "run": {
                        "id": "run-1",
                        "title": "Ship shell",
                        "workflow": "quick-task",
                        "task": "ship",
                        "status": "running",
                        "createdAt": "2026-08-15T00:00:00Z",
                        "tokensUsed": 0,
                        "archived": false,
                        "steps": []
                    }
                }),
            },
            "main",
        );
        assert_eq!(
            run,
            Some(WorkspaceEvent::Run {
                project: "main".to_owned(),
                run: ApiRun {
                    record: coducktor_contract::RunRecord {
                        id: "run-1".to_owned(),
                        title: "Ship shell".to_owned(),
                        workflow: "quick-task".to_owned(),
                        task: "ship".to_owned(),
                        status: coducktor_contract::RunStatus::Running,
                        created_at: "2026-08-15T00:00:00Z".to_owned(),
                        tokens_used: 0.0,
                        archived: false,
                        steps: Vec::new(),
                        ..coducktor_contract::RunRecord::default()
                    },
                    usage: None,
                }
            })
        );

        assert!(
            parse_workspace_event(
                EngineEvent {
                    topic: "workspace".to_owned(),
                    data: serde_json::json!({"type": "provider-status"}),
                },
                "main",
            )
            .is_none()
        );
    }

    #[test]
    fn project_prime_populates_the_sidebar_from_the_full_registry() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        apply_prime_snapshot(
            &mut app,
            PrimeSnapshot {
                health: None,
                runs: None,
                projects: Some(coducktor_contract::ProjectsResponse {
                    projects: vec![coducktor_contract::ProjectListEntry {
                        id: "blarchy".to_owned(),
                        name: "blarchy".to_owned(),
                        root: "/home/przvl/blarchy".to_owned(),
                        ..Default::default()
                    }],
                    boot_project: "blarchy".to_owned(),
                    projects_dir: "~/coducktor/projects".to_owned(),
                }),
                index: None,
                workspace_ui_state: None,
                new_task: PrimeNewTaskSnapshot {
                    config: None,
                    skills: None,
                    workflows: None,
                    workspace_config: None,
                    provider_status: None,
                    ui_state: None,
                    repo: None,
                    branches: Vec::new(),
                },
            },
        );

        assert_eq!(app.projects[0].id, "blarchy");
        assert_eq!(app.project_registry[0].root, "/home/przvl/blarchy");
    }

    #[test]
    fn accepted_run_opens_the_live_thread_before_execution_is_activated() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut starts_in_flight = HashSet::from(["main".to_owned()]);
        let run = coducktor_contract::RunRecord {
            id: "run-live".to_owned(),
            title: "Show live activity".to_owned(),
            workflow: "quick-task".to_owned(),
            task: "Show my prompt and what the agent is doing".to_owned(),
            status: coducktor_contract::RunStatus::Queued,
            created_at: "2026-08-18T00:00:00Z".to_owned(),
            ..coducktor_contract::RunRecord::default()
        };

        apply_started_run(
            &mut app,
            "main".to_owned(),
            Ok(coducktor_contract::CreateRunResponse::Single(Box::new(run))),
            &mut starts_in_flight,
        );

        assert!(matches!(
            app.route(),
            app::Route::Thread { project, id }
                if project == "main" && id == "run-live"
        ));
        assert_eq!(
            app.thread_ui
                .data
                .run
                .as_ref()
                .map(|run| run.record.task.as_str()),
            Some("Show my prompt and what the agent is doing")
        );
        assert!(matches!(
            app.pending.first(),
            Some(PendingAction::ActivateRuns { project }) if project == "main"
        ));
        assert!(
            !app.pending
                .iter()
                .any(|action| matches!(action, PendingAction::LoadThread { .. }))
        );
    }

    #[test]
    fn editor_command_splits_launcher_arguments_without_a_shell() {
        let _ = std::fs::remove_file("/tmp/editor_proof.log");
        let (program, arguments) =
            parse_editor_command("sh -c 'echo handoff: $1 > /tmp/editor_proof.log' sh").unwrap();
        let status = std::process::Command::new(&program)
            .args(&arguments)
            .arg("/home/przvl/blarchy/README.md")
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string("/tmp/editor_proof.log").unwrap(),
            "handoff: /home/przvl/blarchy/README.md\n"
        );
        assert_eq!(
            parse_editor_command("omarchy-launch-editor --inline").unwrap(),
            (
                "omarchy-launch-editor".to_owned(),
                vec!["--inline".to_owned()]
            )
        );
        assert_eq!(
            parse_editor_command("editor --flag 'file mode'").unwrap(),
            (
                "editor".to_owned(),
                vec!["--flag".to_owned(), "file mode".to_owned()]
            )
        );
    }
}
