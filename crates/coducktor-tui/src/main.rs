mod app;
mod input;
mod screens;
mod service;
mod terminal;
mod theme;
mod widgets;

use std::env;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use coducktor_client::{HttpEngine, Scope, SseFrame};
use coducktor_contract::{ApiRun, BackendCheckName};
use crossterm::event::{self, Event, MouseEventKind};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

use crate::app::{App, PendingAction, QuickTask, WorkspaceEvent};
use crate::input::keymap::Keymap;
use crate::service::{ServiceConfig, ServiceState, ServiceSupervisor};
use crate::terminal::AppTerminal;
use crate::theme::Theme;

const FRAME_BUDGET: Duration = Duration::from_millis(33);

#[tokio::main]
async fn main() -> io::Result<()> {
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
    let log_root = env::var_os("DUCK_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".coducktor")))?;
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
            app.history.navigate(crate::app::Route::Tasks {
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
            PendingAction::Quit => {}
        }
    }
}

async fn refresh_index_if_global(engine: &HttpEngine, app: &mut App) {
    if matches!(app.route(), crate::app::Route::GlobalTasks)
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

async fn run(
    terminal: &mut AppTerminal,
    app: &mut App,
    service: &mut Option<ServiceSupervisor>,
    workspace_events: Option<&mut UnboundedReceiver<WorkspaceEvent>>,
) -> io::Result<()> {
    let mut workspace_events = workspace_events;
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
        if let Some(supervisor) = service.as_mut() {
            let _ = supervisor.monitor_once().await;
            app.set_service_state(supervisor.state());
            let engine = supervisor.engine().clone();
            if !app.pending.is_empty() {
                execute_pending(&engine, app).await;
            }
        }
        terminal.draw(|frame| app.render(frame))?;

        let remaining = FRAME_BUDGET.saturating_sub(frame_started.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
    }

    Ok(())
}

fn current_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
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
