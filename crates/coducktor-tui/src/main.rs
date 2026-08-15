mod app;
mod input;
mod service;
mod terminal;
mod theme;

use std::env;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use coducktor_client::HttpEngine;
use crossterm::event::{self, Event, MouseEventKind};

use crate::app::App;
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
        let _ = supervisor.state();
    } else {
        let _ = ServiceState::Disabled;
    }
    let run_result = run(&mut terminal, &mut app, &mut service).await;
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

async fn run(
    terminal: &mut AppTerminal,
    app: &mut App,
    service: &mut Option<ServiceSupervisor>,
) -> io::Result<()> {
    while !app.should_quit() {
        let frame_started = Instant::now();
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
        if let Some(supervisor) = service.as_mut() {
            let _ = supervisor.monitor_once().await;
        }
        terminal.draw(|frame| app.render(frame))?;

        let remaining = FRAME_BUDGET.saturating_sub(frame_started.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
    }

    Ok(())
}
