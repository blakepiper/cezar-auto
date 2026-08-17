use std::env;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use coducktor_client::{HttpEngine, RunStreamEvent, Scope, SseFrame};
use coducktor_contract::{ApiRun, BackendCheckName, TaskSource};
use crossterm::event::{self, Event, MouseEventKind};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

use coducktor_tui::app::{self, App, PendingAction, QuickTask, WorkspaceEvent};
use coducktor_tui::cli::{Cli, Command};
use coducktor_tui::input::keymap::Keymap;
use coducktor_tui::service::{ServiceConfig, ServiceState, ServiceSupervisor};
use coducktor_tui::terminal::AppTerminal;
use coducktor_tui::theme::Theme;
use coducktor_tui::{cli, headless, new_task_form, screens, terminal};

const FRAME_BUDGET: Duration = Duration::from_millis(33);

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse_args();
    // A bad `--repo` is a startup misconfiguration, not a runtime event — reject it
    // before the alternate screen opens, same as a spawn failure (spec §7.7): the
    // TUI never took the screen, so there is nothing to restore.
    if let Some(repo) = &cli.repo
        && !repo.is_dir()
    {
        eprintln!("coducktor: --repo {} is not a directory", repo.display());
        std::process::exit(2);
    }
    // The non-interactive subcommands (B10) never open the alternate screen — they run
    // straight in the caller's terminal, print to real stdout/stderr, and exit. Only
    // `None`/`Tui` fall through to the interactive cockpit below.
    match &cli.command {
        Some(Command::Serve) => {
            let repo_root = headless::resolve_repo_root(cli.repo.as_deref());
            return headless::serve_command(repo_root).await;
        }
        Some(Command::Run { task }) => {
            let repo_root = headless::resolve_repo_root(cli.repo.as_deref());
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
            let repo_root = headless::resolve_repo_root(cli.repo.as_deref());
            headless::init_command(&repo_root);
            return Ok(());
        }
        Some(Command::Usage { .. }) => {
            std::process::exit(headless::usage_command());
        }
        Some(Command::Projects { action }) => {
            let repo_root = headless::resolve_repo_root(cli.repo.as_deref());
            std::process::exit(headless::projects_command(&repo_root, action.clone()));
        }
        None | Some(Command::Tui) => {}
    }
    terminal::install_panic_hook();
    let mut terminal = terminal::setup()?;
    let user_keymap = Keymap::default_path();
    let keymap = Keymap::load(user_keymap.as_deref()).unwrap_or_default();
    let mut app = App::new("main", Theme::detect(), keymap);
    let mut service = configured_service();
    if let Some(supervisor) = service.as_mut() {
        let _ = supervisor.start().await;
        app.set_service_state(supervisor.state());
    } else {
        app.set_service_state(ServiceState::Disabled);
    }
    let mut workspace_listener = None;
    if let Some(engine) = service
        .as_ref()
        .map(|supervisor| supervisor.engine().clone())
    {
        prime_app(&mut app, &engine).await;
        apply_launch_args(&engine, &mut app, &cli).await;
        workspace_listener = open_workspace_listener(engine).await;
    }
    let run_result = run(
        &mut terminal,
        &mut app,
        &mut service,
        workspace_listener.as_mut().map(|(_, receiver)| receiver),
    )
    .await;
    if let Some((handle, _)) = workspace_listener {
        handle.abort();
    }
    if let Some(supervisor) = service.as_mut() {
        supervisor.shutdown().await;
        let _ = supervisor.logs();
    }
    let restore_result = terminal::restore();

    run_result.and(restore_result)
}

fn configured_service() -> Option<ServiceSupervisor> {
    let (command, default_args) = if let Some(command) = env::var_os("DUCK_SERVICE_COMMAND") {
        (PathBuf::from(command), Vec::new())
    } else {
        let entry = discover_service_entry()?;
        (
            PathBuf::from("node"),
            vec![
                "--import".to_owned(),
                "tsx".to_owned(),
                entry.to_string_lossy().into_owned(),
                "serve".to_owned(),
                "--no-open".to_owned(),
            ],
        )
    };
    let base_url =
        env::var("DUCK_SERVICE_URL").unwrap_or_else(|_| "http://127.0.0.1:4321".to_owned());
    let engine = HttpEngine::new(base_url).ok()?;
    let log_root = coducktor_core::paths::coducktor_home_dir(&coducktor_core::paths::ProcessEnv);
    let mut config = ServiceConfig::new(command, log_root.join("logs/service.log"));
    config.args = default_args;
    if let Some(args) = env::var_os("DUCK_SERVICE_ARGS") {
        config.args = args
            .to_string_lossy()
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect();
    }
    Some(ServiceSupervisor::new(config, engine))
}

fn discover_service_entry() -> Option<PathBuf> {
    let packages = env::current_dir().ok()?.join("packages");
    let entries = std::fs::read_dir(packages).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("src/index.ts");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct WorkspaceRunPayload {
    project: String,
    #[serde(flatten)]
    run: ApiRun,
}

#[derive(Debug, Deserialize)]
struct WorkspaceDeletedPayload {
    project: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceTodosPayload {
    project: String,
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceUsagePayload {
    project: String,
    #[serde(default)]
    usage: std::collections::BTreeMap<String, coducktor_contract::ProcessUsage>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceProviderStatusPayload {
    provider: String,
    status: String,
    #[serde(default)]
    enabled: Option<bool>,
}

fn parse_workspace_frame(frame: SseFrame) -> Option<WorkspaceEvent> {
    match frame.event.as_deref()? {
        "run" => {
            let payload = serde_json::from_str::<WorkspaceRunPayload>(&frame.data).ok()?;
            Some(WorkspaceEvent::Run {
                project: payload.project,
                run: payload.run,
            })
        }
        "run-deleted" => {
            let payload = serde_json::from_str::<WorkspaceDeletedPayload>(&frame.data).ok()?;
            Some(WorkspaceEvent::RunDeleted {
                project: payload.project,
                id: payload.id,
            })
        }
        "todos" => {
            let payload = serde_json::from_str::<WorkspaceTodosPayload>(&frame.data).ok()?;
            Some(WorkspaceEvent::Todos {
                project: payload.project,
                count: payload.items.len(),
            })
        }
        "usage" => {
            let payload = serde_json::from_str::<WorkspaceUsagePayload>(&frame.data).ok()?;
            Some(WorkspaceEvent::Usage {
                project: payload.project,
                usage: payload.usage,
            })
        }
        "provider-status" => {
            let payload =
                serde_json::from_str::<WorkspaceProviderStatusPayload>(&frame.data).ok()?;
            Some(WorkspaceEvent::ProviderStatus {
                provider: payload.provider,
                available: payload.status == "connected" && payload.enabled != Some(false),
            })
        }
        _ => None,
    }
}

async fn prime_app(app: &mut App, engine: &HttpEngine) {
    if let Ok(health) = engine.health().await {
        // Adopt the boot project the service actually knows about — the TUI's
        // "main" default is only a placeholder until the health answer arrives.
        if !health.boot_project.is_empty()
            && app.projects.iter().all(|p| p.id != health.boot_project)
        {
            app.history.navigate(app::Route::Tasks {
                project: health.boot_project.clone(),
            });
            app.default_project = health.boot_project.clone();
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
        app.new_task_ui.data.repo = health.repo.clone();
    }
    let project = app.current_project().to_owned();
    if let Ok(runs) = engine.list_runs(&Scope::Project(project.clone())).await {
        app.set_tasks(runs);
        app.set_quick_tasks(
            app.tasks
                .iter()
                .map(|run| QuickTask::from_api(project.clone(), run.clone()))
                .collect::<Vec<_>>(),
        );
    }
    if let Ok(projects) = engine.projects().await {
        app.set_project_registry(projects.projects);
    }
    if let Ok(index) = engine.runs_index().await {
        app.set_global_index(index);
    }
    if let Ok(state) = engine.workspace_ui_state().await {
        app.notifications_enabled = state
            .notifications
            .as_ref()
            .and_then(|notifications| notifications.enabled)
            .unwrap_or(false);
    }
    let project = app.current_project().to_owned();
    refresh_new_task(engine, app, &project).await;
}

/// Apply `--repo`/`--workflow`/`--model` (spec §10 A13) once `prime_app` has loaded
/// the project registry. `--repo` switches the active project — re-fetching its
/// tasks and New Task data if it differs from the one `prime_app` already loaded —
/// or leaves a clear notice if the directory isn't a registered project rather than
/// silently staying put. `--workflow`/`--model` preselect the New Task screen,
/// covering the same "hand a task to the agent from outside the TUI" use case the
/// deleted browser-launch surface used to (spec §9.3 point 2).
async fn apply_launch_args(engine: &HttpEngine, app: &mut App, cli: &Cli) {
    if let Some(repo) = &cli.repo {
        match cli::resolve_repo(&app.project_registry, repo) {
            Some(project) => {
                if project != app.default_project {
                    app.default_project = project.clone();
                    refresh_tasks(engine, app, &project).await;
                    refresh_new_task(engine, app, &project).await;
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
/// server's answer. Failures surface as a toast rather than a crash.
async fn execute_pending(engine: &HttpEngine, app: &mut App) {
    for action in app.take_pending() {
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
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("archive failed: {error}")),
                }
            }
            PendingAction::Delete { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.delete_run(&scope, &id).await {
                    Ok(_) => {
                        app.apply_workspace_event(WorkspaceEvent::RunDeleted { project, id });
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("delete failed: {error}")),
                }
            }
            PendingAction::Read { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.read_run(&scope, &id).await {
                    Ok(run) => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("mark read failed: {error}")),
                }
            }
            PendingAction::Unread { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.unread_run(&scope, &id).await {
                    Ok(run) => {
                        app.apply_workspace_event(WorkspaceEvent::Run { project, run });
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("mark unread failed: {error}")),
                }
            }
            PendingAction::ArchiveFinished { project } => {
                let scope = Scope::Project(project.clone());
                match engine.archive_finished(&scope).await {
                    Ok(response) => {
                        app.notice = Some(format!("archived {} finished", response.archived));
                        refresh_tasks(engine, app, &project).await;
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("archive finished failed: {error}")),
                }
            }
            PendingAction::MarkAllRead { project } => {
                let scope = Scope::Project(project.clone());
                match engine.mark_all_read(&scope).await {
                    Ok(response) => {
                        app.notice = Some(format!("marked {} read", response.read));
                        refresh_tasks(engine, app, &project).await;
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("mark all read failed: {error}")),
                }
            }
            PendingAction::RefreshTasks { project } => {
                refresh_tasks(engine, app, &project).await;
            }
            PendingAction::RefreshIndex => {
                if let Ok(index) = engine.runs_index().await {
                    app.set_global_index(index);
                }
            }
            PendingAction::StartRun { project, input } => {
                let scope = Scope::Project(project.clone());
                match engine.start_run(&scope, &input).await {
                    Ok(response) => {
                        if let Some(id) = new_task_form::started_run_id(&response) {
                            screens::thread::open(app, &project, &id);
                        }
                        screens::new_task::clear_draft(app);
                        refresh_tasks(engine, app, &project).await;
                        refresh_index_if_global(engine, app).await;
                    }
                    Err(error) => app.notice = Some(format!("start failed: {error}")),
                }
            }
            PendingAction::RefreshNewTask { project } => {
                refresh_new_task(engine, app, &project).await;
            }
            PendingAction::RefreshModels { runner } => match engine.models(runner).await {
                Ok(catalog) => {
                    app.new_task_ui.data.model_catalog = Some(catalog);
                }
                Err(error) => {
                    app.notice = Some(format!("model catalog failed: {error}"));
                }
            },
            PendingAction::PlanTask { project, task } => {
                let scope = Scope::Project(project.clone());
                match engine.plan(&scope, &task).await {
                    Ok(plan) => {
                        app.new_task_ui.plan = Some(plan);
                    }
                    Err(error) => {
                        app.notice = Some(format!("plan failed: {error}"));
                        app.new_task_ui.plan_visible = false;
                    }
                }
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
                let run = engine.get_run(&scope, &id).await;
                let history = engine.run_history(&scope, &id, None).await;
                match (run, history) {
                    (Ok(run), Ok(history)) => {
                        let events = history
                            .events
                            .into_iter()
                            .map(thread_history_event)
                            .collect();
                        app.thread_ui
                            .load(project, id, run, events, history.as_of_seq as f64);
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        app.notice = Some(format!("load task failed: {error}"));
                    }
                }
            }
            PendingAction::SendMessage { project, id, input } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.send_message(&scope, &id, &input).await {
                    app.notice = Some(format!("send failed: {error}"));
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::CancelRun { project, id } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.cancel_run(&scope, &id).await {
                    app.notice = Some(format!("cancel failed: {error}"));
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::ContinueRun { project, id, text } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::ContinueInput {
                    text,
                    ..coducktor_contract::ContinueInput::default()
                };
                if let Err(error) = engine.continue_run(&scope, &id, &input).await {
                    app.notice = Some(format!("continue failed: {error}"));
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::FinishRun { project, id } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.finish_run(&scope, &id).await {
                    app.notice = Some(format!("finish failed: {error}"));
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::CreatePr { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.create_pr(&scope, &id).await {
                    Ok(response) => {
                        app.notice = Some(format!("draft PR created — {}", response.url))
                    }
                    Err(error) => app.notice = Some(format!("draft PR failed: {error}")),
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::OpenInCli { project, id } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.open_in_cli(&scope, &id).await {
                    app.notice = Some(format!("open in terminal failed: {error}"));
                }
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
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::CancelAutoResume { project, id } => {
                let scope = Scope::Project(project.clone());
                if let Err(error) = engine.cancel_auto_resume(&scope, &id).await {
                    app.notice = Some(format!("cancel auto-resume failed: {error}"));
                }
                refresh_thread_run(engine, app, &project, &id).await;
            }
            PendingAction::LoadTaskGitChanges { project, id } => {
                let scope = Scope::Project(project.clone());
                let run = engine.get_run(&scope, &id).await;
                let changes = engine.run_changes(&scope, &id).await;
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
            PendingAction::LoadTaskGitFiles { project, id, path } => {
                let scope = Scope::Project(project.clone());
                match engine.run_files(&scope, &id, path.as_deref()).await {
                    Ok(entry) => app.task_git_ui.files_entry = Some(entry),
                    Err(error) => app.notice = Some(format!("load files failed: {error}")),
                }
            }
            PendingAction::LoadTaskGitCommits { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.run_commits(&scope, &id).await {
                    Ok(commits) => app.task_git_ui.commits = Some(commits),
                    Err(error) => app.notice = Some(format!("load commits failed: {error}")),
                }
            }
            PendingAction::LoadTaskGitCommitDiff { project, id, sha } => {
                let scope = Scope::Project(project.clone());
                match engine.run_commit(&scope, &id, &sha).await {
                    Ok(commit) => app.task_git_ui.commit_detail = Some(commit),
                    Err(error) => app.notice = Some(format!("load commit failed: {error}")),
                }
            }
            PendingAction::TaskGitCommit { project, id } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::GitCommitInput {
                    message: app.task_git_ui.commit_message.clone(),
                };
                match engine.git_commit(&scope, &id, &input).await {
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
                match engine.repo(&scope).await {
                    Ok(repo) => app.repo_git_ui.repo = Some(repo),
                    Err(error) => app.notice = Some(format!("load repo failed: {error}")),
                }
                if let Ok(changes) = engine.repo_changes(&scope).await {
                    app.repo_git_ui.repo_changes_files = changes.files;
                }
            }
            PendingAction::LoadRepoGitCommits { project } => {
                let scope = Scope::Project(project.clone());
                match engine.repo(&scope).await {
                    Ok(repo) => app.repo_git_ui.repo = Some(repo),
                    Err(error) => app.notice = Some(format!("load repo failed: {error}")),
                }
            }
            PendingAction::LoadRepoGitCommitDiff { project, sha } => {
                let scope = Scope::Project(project.clone());
                match engine.repo_commit(&scope, &sha).await {
                    Ok(commit) => app.repo_git_ui.commit_detail = Some(commit),
                    Err(error) => app.notice = Some(format!("load commit failed: {error}")),
                }
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
                match engine.group(&scope, &group_id).await {
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
            PendingAction::LoadCompareVariantDiff {
                project,
                group_id: _,
                run_id,
            } => {
                let scope = Scope::Project(project.clone());
                match engine.run_changes(&scope, &run_id).await {
                    Ok(changes) => {
                        app.compare_ui.variant_diffs.insert(run_id, changes);
                    }
                    Err(error) => app.notice = Some(format!("load diff failed: {error}")),
                }
            }
            PendingAction::PickVariant {
                project,
                group_id,
                run_id,
            } => {
                let scope = Scope::Project(project.clone());
                let input = coducktor_contract::PickVariantRequest { run_id };
                match engine.pick_variant(&scope, &group_id, &input).await {
                    Ok(_) => app.notice = Some("variant picked".to_owned()),
                    Err(error) => app.notice = Some(format!("pick failed: {error}")),
                }
                refresh_tasks(engine, app, &project).await;
            }
            PendingAction::LoadIdeDirectory { project, path } => {
                let scope = Scope::Project(project.clone());
                // The listed directory IS the screen's current directory — the sidebar entry
                // point queues a root listing, GoUp/Enter queue a subdirectory, and the state
                // must converge on the same path the header renders.
                app.ide_ui.directory_path = path.clone().unwrap_or_default();
                match engine.ide_tree(&scope, path.as_deref()).await {
                    Ok(directory) => {
                        app.ide_ui.entries = Some(directory);
                        app.ide_ui.tree_selected = 0;
                    }
                    Err(error) => app.notice = Some(format!("load directory failed: {error}")),
                }
            }
            PendingAction::LoadIdeFile { project, path } => {
                let scope = Scope::Project(project.clone());
                match engine.ide_file(&scope, &path).await {
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
                    Err(error) => {
                        app.ide_ui.file_error = Some(error.to_string());
                    }
                }
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
                let absolute = app
                    .project_registry
                    .iter()
                    .find(|entry| entry.id == project)
                    .map(|entry| {
                        if path.is_empty() {
                            entry.root.clone()
                        } else {
                            format!("{}/{}", entry.root.trim_end_matches('/'), path)
                        }
                    });
                match absolute {
                    Some(absolute) => app.set_editor_handoff(absolute),
                    None => {
                        app.notice = Some("project root unknown — cannot open in editor".to_owned())
                    }
                }
            }
            PendingAction::IdeDiscardThenNavigate(_) => unreachable!("resolved in app.rs"),
            PendingAction::IdeDiscardThenBack => unreachable!("resolved in app.rs"),
            PendingAction::IdeDiscardThenForward => unreachable!("resolved in app.rs"),
            PendingAction::LoadGithub { project } => {
                let scope = Scope::Project(project.clone());
                match engine.github(&scope).await {
                    Ok(data) => {
                        if !data.available {
                            app.github_ui.data = Some(data.clone());
                        } else {
                            app.github_ui.data = Some(data);
                        }
                    }
                    Err(error) => app.notice = Some(format!("load github failed: {error}")),
                }
            }
            PendingAction::LoadGithubPickers { project } => {
                let scope = Scope::Project(project.clone());
                if let Ok(workflows) = engine.workflows(&scope).await {
                    app.github_ui.workflows = workflows.workflows;
                }
                if let Ok(skills) = engine.skills(&scope).await {
                    app.github_ui.skills = skills;
                }
            }
            PendingAction::LoadGithubComments {
                project,
                kind,
                number,
            } => {
                let scope = Scope::Project(project.clone());
                match engine.github_comments(&scope, &kind, number).await {
                    Ok(comments) => app.github_ui.comments = Some(comments),
                    Err(error) => app.notice = Some(format!("load comments failed: {error}")),
                }
            }
            PendingAction::LoadGithubMergeState { project, number } => {
                let scope = Scope::Project(project.clone());
                match engine.github_pr_merge_state(&scope, number).await {
                    Ok(state) => app.github_ui.merge_state = Some(state),
                    Err(error) => app.notice = Some(format!("load merge state failed: {error}")),
                }
            }
            PendingAction::LoadGithubPrChanges { project, number } => {
                let scope = Scope::Project(project.clone());
                match engine.github_pr_changes(&scope, number).await {
                    Ok(changes) => app.github_ui.pr_changes = Some(changes),
                    Err(error) => app.notice = Some(format!("load changes failed: {error}")),
                }
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
                match engine.github_merge_pr(&scope, number, &input).await {
                    Ok(response) => {
                        app.notice = Some(format!("merged PR #{number} with {}", response.method));
                        app.pending.push(PendingAction::LoadGithub { project });
                    }
                    Err(error) => app.notice = Some(format!("merge failed: {error}")),
                }
            }
            PendingAction::GithubHandToAgent { project, input } => {
                let scope = Scope::Project(project.clone());
                match engine.start_run(&scope, &input).await {
                    Ok(response) => {
                        if let Some(id) = new_task_form::started_run_id(&response) {
                            app.github_ui.queued = Some(id);
                        }
                        refresh_tasks(engine, app, &project).await;
                    }
                    Err(error) => app.notice = Some(format!("start failed: {error}")),
                }
            }
            PendingAction::LoadInbox { project } => {
                let scope = Scope::Project(project.clone());
                match engine.health().await {
                    Ok(health) => {
                        app.inbox_ui.followups_enabled = Some(health.capabilities.followups);
                    }
                    Err(error) => app.notice = Some(format!("load health failed: {error}")),
                }
                match engine.todos(&scope).await {
                    Ok(todos) => app.inbox_ui.todos = Some(todos),
                    Err(error) => app.notice = Some(format!("load inbox failed: {error}")),
                }
            }
            PendingAction::StartTodo { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.start_todo(&scope, &id).await {
                    Ok(_) => {
                        app.notice = Some("todo started — see Tasks".to_owned());
                        refresh_tasks(engine, app, &project).await;
                        app.pending.push(PendingAction::LoadInbox { project });
                    }
                    Err(error) => app.notice = Some(format!("start failed: {error}")),
                }
            }
            PendingAction::DismissTodo { project, id } => {
                let scope = Scope::Project(project.clone());
                match engine.delete_todo(&scope, &id).await {
                    Ok(_) => {
                        app.pending.push(PendingAction::LoadInbox { project });
                    }
                    Err(error) => app.notice = Some(format!("dismiss failed: {error}")),
                }
            }
            PendingAction::LoadSkills { project } => {
                let scope = Scope::Project(project.clone());
                match engine.skills(&scope).await {
                    Ok(skills) => app.skills_ui.skills = skills,
                    Err(error) => app.notice = Some(format!("load skills failed: {error}")),
                }
            }
            PendingAction::LoadWorkflows { project } => {
                let scope = Scope::Project(project.clone());
                match engine.workflows(&scope).await {
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
            PendingAction::LoadWorkflowSkills { project } => {
                let scope = Scope::Project(project.clone());
                match engine.skills(&scope).await {
                    Ok(skills) => app.workflows_ui.palette_skills = skills,
                    Err(error) => app.notice = Some(format!("load skills failed: {error}")),
                }
            }
            PendingAction::SaveWorkflow { project } => {
                save_or_export_workflow(engine, app, &project, false).await;
            }
            PendingAction::ExportWorkflow { project } => {
                save_or_export_workflow(engine, app, &project, true).await;
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
                load_settings(engine, app, &project).await;
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
                let scope = Scope::Project(project);
                match engine.agent_config_file(&scope, &id).await {
                    Ok(file) => {
                        app.settings_ui.file_editor.set_text(&file.content);
                        app.settings_ui.open_file = Some(file);
                        app.settings_ui.file_editing = true;
                    }
                    Err(error) => app.notice = Some(format!("agent config: {error}")),
                }
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
                    if let Ok(projects) = engine.projects().await {
                        app.set_project_registry(projects.projects);
                    }
                }
                Err(error) => app.notice = Some(format!("remove project failed: {error}")),
            },
            PendingAction::SettingsUpdateProject { id, input } => {
                match engine.update_project(&id, &input).await {
                    Ok(_) => {
                        if let Ok(projects) = engine.projects().await {
                            app.set_project_registry(projects.projects);
                        }
                    }
                    Err(error) => app.notice = Some(format!("update project failed: {error}")),
                }
            }
            PendingAction::Quit => {}
        }
    }
}

/// Every Settings data source, in one place — the section list needs all of it at once
/// rather than per-section lazy loads, since Tab cycling between sections must not each
/// re-trigger a fetch.
async fn load_settings(engine: &HttpEngine, app: &mut App, project: &str) {
    let scope = Scope::Project(project.to_owned());
    if let Ok(config) = engine.config(&scope).await {
        app.settings_ui.config = Some(config);
    }
    if let Ok(config) = engine.workspace_config().await {
        app.settings_ui.workspace_config = Some(config);
    }
    if let Ok(state) = engine.workspace_ui_state().await {
        app.notifications_enabled = state
            .notifications
            .as_ref()
            .and_then(|notifications| notifications.enabled)
            .unwrap_or(false);
        app.settings_ui.workspace_ui_state = Some(state);
    }
    if let Ok(state) = engine.ui_state(&scope).await {
        app.settings_ui.ui_state = Some(state);
    }
    if let Ok(listing) = engine.agent_config(&scope).await {
        app.settings_ui.agent_config = Some(listing);
    }
    if let Ok(profiles) = engine.agent_profiles().await {
        app.settings_ui.agent_profiles = Some(profiles);
    }
    if let Ok(worktrees) = engine.worktrees(&scope).await {
        app.settings_ui.worktrees = Some(worktrees);
    }
}

/// Every thread-mutating action re-fetches the run record so the header/actions/dock reflect
/// the server's answer immediately, rather than waiting for the next workspace `run` frame.
async fn refresh_thread_run(engine: &HttpEngine, app: &mut App, project: &str, id: &str) {
    if app.thread_ui.data.project != project || app.thread_ui.data.run_id != id {
        return;
    }
    let scope = Scope::Project(project.to_owned());
    if let Ok(run) = engine.get_run(&scope, id).await {
        app.thread_ui.set_run(run);
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

async fn refresh_index_if_global(engine: &HttpEngine, app: &mut App) {
    if matches!(app.route(), app::Route::GlobalTasks)
        && let Ok(index) = engine.runs_index().await
    {
        app.set_global_index(index);
    }
}

async fn refresh_tasks(engine: &HttpEngine, app: &mut App, project: &str) {
    let scope = Scope::Project(project.to_owned());
    if let Ok(runs) = engine.list_runs(&scope).await {
        app.set_tasks(runs);
        app.set_quick_tasks(
            app.tasks
                .iter()
                .map(|run| QuickTask::from_api(project.to_owned(), run.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

/// Load everything the New Task screen reads (§8.3), scoped to the active project.
async fn refresh_new_task(engine: &HttpEngine, app: &mut App, project: &str) {
    let scope = Scope::Project(project.to_owned());
    if let Ok(config) = engine.config(&scope).await {
        app.new_task_ui.data.config = Some(new_task_form::ComposerConfig::from_config(&config));
    }
    if let Ok(skills) = engine.skills(&scope).await {
        app.new_task_ui.data.skills = skills;
    }
    if let Ok(workflows) = engine.workflows(&scope).await {
        app.new_task_ui.data.workflows = workflows.workflows;
    }
    if let Ok(workspace_config) = engine.workspace_config().await {
        app.new_task_ui.data.workspace_config = Some(workspace_config);
    }
    if let Ok(provider_status) = engine.provider_status().await {
        app.new_task_ui.data.provider_status = Some(provider_status);
    }
    if let Ok(agent_profiles) = engine.agent_profiles().await {
        app.new_task_ui.data.agent_profiles = Some(agent_profiles);
    }
    if let Ok(ui_state) = engine.ui_state(&scope).await {
        app.new_task_ui.data.ui_state = Some(ui_state);
    }
}

async fn open_workspace_listener(
    engine: HttpEngine,
) -> Option<(JoinHandle<()>, UnboundedReceiver<WorkspaceEvent>)> {
    let (sender, receiver) = unbounded_channel();
    let handle = tokio::spawn(async move {
        loop {
            let Ok(mut frames) = engine
                .sse_frames(&Scope::Workspace, "/workspace/events")
                .await
            else {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            while let Some(frame) = frames.next().await {
                if let Ok(frame) = frame
                    && let Some(event) = parse_workspace_frame(frame)
                    && sender.send(event).is_err()
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
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

/// The currently open thread's live event stream (§8.4 A8) — opened when the route enters
/// `Route::Thread`, aborted the moment it leaves. Unlike the workspace listener (one for the
/// whole session), this one is per-navigation.
struct ThreadListener {
    project: String,
    id: String,
    handle: JoinHandle<()>,
    receiver: UnboundedReceiver<RunStreamEvent>,
}

async fn open_run_listener(engine: HttpEngine, project: String, id: String) -> ThreadListener {
    let (sender, receiver) = unbounded_channel();
    let handle = tokio::spawn({
        let project = project.clone();
        let id = id.clone();
        async move {
            let scope = Scope::Project(project);
            let Ok(mut stream) = engine.run_events(&scope, &id, None, None).await else {
                return;
            };
            while let Some(frame) = stream.next().await {
                if let Ok(frame) = frame
                    && sender.send(frame).is_err()
                {
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
    }
}

async fn run(
    terminal: &mut AppTerminal,
    app: &mut App,
    service: &mut Option<ServiceSupervisor>,
    workspace_events: Option<&mut UnboundedReceiver<WorkspaceEvent>>,
) -> io::Result<()> {
    let mut workspace_events = workspace_events;
    let mut thread_listener: Option<ThreadListener> = None;
    let mut last_needs_you = usize::MAX;
    while !app.should_quit() {
        let frame_started = Instant::now();
        app.now_epoch = current_epoch_seconds();
        let mut pending_mouse = None;
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => {
                    pending_mouse = Some(Event::Mouse(mouse));
                }
                event => app.handle_event(event),
            }
        }
        if let Some(mouse) = pending_mouse {
            app.handle_event(mouse);
        }
        if let Some(events) = workspace_events.as_deref_mut() {
            while let Ok(event) = events.try_recv() {
                app.apply_workspace_event(event);
            }
        }
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
            if let (Some((project, id)), Some(supervisor)) = (desired_thread, service.as_ref()) {
                let engine = supervisor.engine().clone();
                thread_listener = Some(open_run_listener(engine, project, id).await);
            }
        }
        if let Some(listener) = thread_listener.as_mut() {
            while let Ok(frame) = listener.receiver.try_recv() {
                if app.thread_ui.data.project == listener.project
                    && app.thread_ui.data.run_id == listener.id
                {
                    app.thread_ui.push_event(frame.seq, frame.event);
                }
            }
        }
        if let Some(supervisor) = service.as_mut() {
            let _ = supervisor.monitor_once().await;
            app.set_service_state(supervisor.state());
            if app.logs_open {
                app.set_service_logs(supervisor.logs());
            }
            let engine = supervisor.engine().clone();
            if !app.pending.is_empty() {
                execute_pending(&engine, app).await;
            }
        }
        for (summary, body) in app.take_pending_notifications() {
            coducktor_tui::notify::notify(app.notifications_enabled, &summary, &body);
        }
        let needs_you = app.needs_you_count();
        if needs_you != last_needs_you {
            coducktor_tui::notify::set_title(&coducktor_tui::notify::title_for(needs_you));
            last_needs_you = needs_you;
        }
        // The IDE's `Ctrl+E` escape hatch (spec §8.8): main owns the terminal, so the
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
        terminal.draw(|frame| app.render(frame))?;

        let remaining = FRAME_BUDGET.saturating_sub(frame_started.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
    }
    if let Some(listener) = thread_listener {
        listener.handle.abort();
    }

    Ok(())
}

/// Save or export the workflows draft. Both write `POST /workflows`; the export path is the
/// same write answered with the file path it landed in (spec §8.13). The body honors the
/// portable compact form: `skills:` when every step is a plain skill step, `steps:` otherwise
/// (mirrors the server's own `skillStackOf`, spec 012 — a protected format property).
async fn save_or_export_workflow(engine: &HttpEngine, app: &mut App, project: &str, export: bool) {
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
    // The portable compact form is XOR with the full form (the server's schema: "provide
    // either steps or skills, not both").
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
/// file in the real terminal, then re-enter raw mode and the alternate screen. The
/// supervised `cezar serve` child keeps its piped stdout throughout, so nothing foreign
/// reaches the terminal (one-terminal rule, §7.7).
fn run_editor_handoff(terminal: &mut AppTerminal, path: &str) -> io::Result<()> {
    use crossterm::cursor;
    use crossterm::event::EnableMouseCapture;
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};

    terminal.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;

    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());
    let result = std::process::Command::new(&editor).arg(path).status();

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
        .map_err(|error| io::Error::other(format!("failed to run {editor}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_sse_frames_decode_shell_badges() {
        let run = parse_workspace_frame(SseFrame {
            id: None,
            event: Some("run".to_owned()),
            data: r#"{"project":"main","id":"run-1","title":"Ship shell","workflow":"quick-task","task":"ship","status":"running","createdAt":"2026-08-15T00:00:00Z","tokensUsed":0,"archived":false,"steps":[]}"#.to_owned(),
        });
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

        let usage = parse_workspace_frame(SseFrame {
            id: None,
            event: Some("usage".to_owned()),
            data: r#"{"project":"main","usage":{"run-1":{"cpuPct":37.5,"rssBytes":1048576,"procCount":3}}}"#
                .to_owned(),
        });
        assert_eq!(
            usage,
            Some(WorkspaceEvent::Usage {
                project: "main".to_owned(),
                usage: [(
                    "run-1".to_owned(),
                    coducktor_contract::ProcessUsage {
                        cpu_pct: 37.5,
                        rss_bytes: 1048576.0,
                        proc_count: 3.0,
                    }
                )]
                .into_iter()
                .collect(),
            })
        );

        let todo = parse_workspace_frame(SseFrame {
            id: None,
            event: Some("todos".to_owned()),
            data: r#"{"project":"main","items":[{"summary":"one"},{"summary":"two"}]}"#.to_owned(),
        });
        assert_eq!(
            todo,
            Some(WorkspaceEvent::Todos {
                project: "main".to_owned(),
                count: 2,
            })
        );

        let provider = parse_workspace_frame(SseFrame {
            id: None,
            event: Some("provider-status".to_owned()),
            data: r#"{"provider":"codex","status":"connected","enabled":false}"#.to_owned(),
        });
        assert_eq!(
            provider,
            Some(WorkspaceEvent::ProviderStatus {
                provider: "codex".to_owned(),
                available: false,
            })
        );
    }
}
