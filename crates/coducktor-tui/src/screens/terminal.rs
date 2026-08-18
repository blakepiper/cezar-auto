//! The project Terminal screen. A real shell runs inside the cockpit in an embedded
//! PTY (`crate::pty::TerminalSession`) — the cockpit keeps ownership of its alternate
//! screen and raw input mode, and every keystroke on this tab goes to the shell.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, Route};
use crate::pty::{TerminalSession, encode_key};

/// The terminal shell state shared by content screens.
#[derive(Default)]
pub struct TerminalUi {
    /// The project whose terminal tab is (or was last) open.
    pub project: String,
    /// One embedded shell per project, kept alive across navigation.
    pub sessions: BTreeMap<String, TerminalSession>,
    /// Why the shell could not start (unknown root, spawn failure). Cleared on relaunch.
    pub error: Option<String>,
    /// Scrollback offset in rows; `0` shows the live screen bottom.
    pub scroll_offset: usize,
    /// The tab's inner rect from the last render, used to size the PTY.
    pub(crate) last_area: Option<Rect>,
}

/// Open the Terminal tab for a project. The shell itself starts lazily in
/// `maintain`, which runs from the main loop once per frame.
pub fn open(app: &mut App, project: &str) {
    if app.terminal_ui.project != project {
        app.terminal_ui.project = project.to_owned();
        app.terminal_ui.error = None;
    }
    app.request_navigate(Route::Terminal {
        project: project.to_owned(),
    });
}

fn sync_from_route(app: &mut App) {
    if let Route::Terminal { project } = app.route()
        && app.terminal_ui.project != *project
    {
        app.terminal_ui.project = project.clone();
        app.terminal_ui.error = None;
    }
}

/// Called from the run loop once per frame while the cockpit runs: starts the shell
/// for the open terminal tab on first visit, keeps the PTY size and scroll offset in
/// sync, and reports whether the terminal tab is the active route (the caller uses
/// that to scope bracketed paste). Never touches the outer terminal in tests — the
/// bracketed-paste toggle is the caller's job.
pub fn maintain(app: &mut App) -> bool {
    sync_from_route(app);
    let project = match app.route() {
        Route::Terminal { project } => project.clone(),
        _ => return false,
    };
    let ui = &mut app.terminal_ui;
    if !ui.sessions.contains_key(&project) && ui.error.is_none() {
        let root = app
            .project_registry
            .iter()
            .find(|entry| entry.id == project)
            .map(|entry| PathBuf::from(&entry.root))
            .or_else(|| {
                (project == app.default_project)
                    .then(|| app.boot_root.clone())
                    .flatten()
            });
        match root {
            Some(root) => {
                let (rows, cols) = ui
                    .last_area
                    .map_or((24, 80), |area| (area.height, area.width));
                match TerminalSession::spawn(&root, rows, cols) {
                    Ok(session) => {
                        ui.sessions.insert(project.clone(), session);
                    }
                    Err(error) => {
                        ui.error = Some(format!(
                            "could not start a shell at {}: {error}",
                            root.display()
                        ));
                    }
                }
            }
            None => {
                ui.error =
                    Some("the project root is not known yet — try again in a moment".to_owned());
            }
        }
    }
    if let Some(session) = ui.sessions.get_mut(&project) {
        if let Some(area) = ui.last_area {
            session.resize(area.height, area.width);
        }
        session.set_scrollback(ui.scroll_offset);
    }
    true
}

/// Discard the current session so the next frame starts a fresh shell.
fn relaunch(app: &mut App) {
    let project = app.terminal_ui.project.clone();
    app.terminal_ui.sessions.remove(&project);
    app.terminal_ui.error = None;
    app.terminal_ui.scroll_offset = 0;
}

/// Scroll the embedded terminal's scrollback. `up` moves toward older output.
pub fn scroll(app: &mut App, up: bool) {
    if up {
        app.terminal_ui.scroll_offset = app.terminal_ui.scroll_offset.saturating_add(3);
    } else {
        app.terminal_ui.scroll_offset = app.terminal_ui.scroll_offset.saturating_sub(3);
    }
    let project = app.terminal_ui.project.clone();
    if let Some(session) = app.terminal_ui.sessions.get(&project) {
        session.set_scrollback(app.terminal_ui.scroll_offset);
    }
}

/// Deliver a bracketed-paste chunk into the shell.
pub fn paste(app: &mut App, text: &str) {
    let project = app.terminal_ui.project.clone();
    if let Some(session) = app.terminal_ui.sessions.get_mut(&project)
        && !session.exited()
    {
        let _ = session.write_bytes(text.as_bytes());
        app.terminal_ui.scroll_offset = 0;
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    sync_from_route(app);
    let project = app.terminal_ui.project.clone();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("TERMINAL — {project}"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.terminal_ui.last_area = Some(inner);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(session) = app.terminal_ui.sessions.get(&project) else {
        if let Some(error) = &app.terminal_ui.error {
            render_message(
                frame,
                inner,
                app,
                "Could not start a shell",
                error,
                "Press Enter or r to try again.",
            );
        } else {
            render_message(
                frame,
                inner,
                app,
                "Starting a shell",
                "The shell opens inside this tab at the project root.",
                "Press Enter or r to start it now.",
            );
        }
        return;
    };

    render_screen(frame, inner, app, session);
    render_status(frame, inner, app, session);
}

fn render_message(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    body: &str,
    hint: &str,
) {
    let lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(app.theme.palette.failed)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(body.to_owned()),
        Line::from(""),
        Line::from(hint.to_owned()),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Render the parser's visible grid (the scrollback offset is already applied to the
/// parser) with per-cell colors and a reversed cursor block.
fn render_screen(frame: &mut Frame<'_>, area: Rect, app: &App, session: &TerminalSession) {
    let rows = usize::from(area.height);
    let cols = usize::from(area.width);
    let grid_height = rows.saturating_sub(1);
    let Ok(parser) = session.parser().lock() else {
        return;
    };
    let screen = parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let offset = screen.scrollback();
    let mut lines = Vec::with_capacity(grid_height);
    for row in 0..grid_height {
        let mut spans = Vec::new();
        for col in 0..cols {
            let Some(cell) = screen.cell(row as u16, col as u16) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let is_cursor = row == usize::from(cursor_row).saturating_add(offset)
                && col == usize::from(cursor_col);
            let mut contents = cell.contents().to_owned();
            if contents.is_empty() {
                if !is_cursor && cell.bgcolor() == vt100::Color::Default {
                    continue;
                }
                contents.push(' ');
            }
            let mut style = Style::default()
                .fg(cell_color(cell.fgcolor(), app.theme.palette.fg))
                .bg(cell_color(cell.bgcolor(), app.theme.palette.bg));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if is_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(contents, style));
        }
        lines.push(Line::from(spans));
    }
    let grid = Rect::new(area.x, area.y, area.width, grid_height as u16);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.theme.palette.bg)),
        grid,
    );
}

fn cell_color(color: vt100::Color, fallback: ratatui::style::Color) -> ratatui::style::Color {
    match color {
        vt100::Color::Default => fallback,
        vt100::Color::Idx(index) => ratatui::style::Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => ratatui::style::Color::Rgb(red, green, blue),
    }
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App, session: &TerminalSession) {
    let status = if session.exited() {
        format!("shell exited — Enter or r restarts  ·  {}", session.cwd())
    } else {
        let mut status = session.cwd().to_string();
        if app.terminal_ui.scroll_offset > 0 {
            status.push_str(&format!("  ·  {} lines up", session.scrollback()));
        }
        status.push_str("  ·  Ctrl+Left to leave");
        status
    };
    let style = Style::default().fg(app.theme.palette.soft_fg);
    let row = area.bottom().saturating_sub(1);
    let rect = Rect::new(area.x, row, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, style))),
        rect,
    );
}

/// Forward every key to the shell while it is alive. A dead or unstarted shell falls
/// back to the degraded screen: Enter/r restarts, everything else keeps its app
/// meaning (Esc leaves the tab, q quits, and so on).
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    let project = app.terminal_ui.project.clone();
    let Some(session) = app.terminal_ui.sessions.get_mut(&project) else {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('r') => {
                relaunch(app);
                true
            }
            _ => false,
        };
    };
    if session.exited() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('r') => {
                relaunch(app);
                true
            }
            _ => false,
        };
    }
    if let Some(bytes) = encode_key(key) {
        let _ = session.write_bytes(&bytes);
    }
    app.terminal_ui.scroll_offset = 0;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app_with_terminal(project: &str) -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_project_registry(vec![coducktor_contract::ProjectListEntry {
            id: project.to_owned(),
            name: project.to_owned(),
            root: "/tmp/project".to_owned(),
            ..coducktor_contract::ProjectListEntry::default()
        }]);
        open(&mut app, project);
        app
    }

    #[test]
    fn opening_the_terminal_tab_navigates_without_queuing_a_launch() {
        let mut app = app_with_terminal("main");
        open(&mut app, "main");

        assert!(matches!(app.route(), Route::Terminal { project } if project == "main"));
        assert!(app.pending.is_empty());
    }

    #[test]
    fn an_open_command_navigation_adopts_the_routes_project() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.execute_command("open /p/coducktor/terminal");
        assert!(matches!(
            app.route(),
            Route::Terminal { project } if project == "coducktor"
        ));
    }

    #[test]
    fn maintain_spawns_a_session_for_a_registered_project_root() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        assert!(app.terminal_ui.sessions.contains_key("main"));
        assert!(app.terminal_ui.error.is_none());
    }

    #[test]
    fn maintain_reports_inactive_away_from_the_terminal_route() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        assert!(!maintain(&mut app));
    }

    #[test]
    fn maintain_reports_an_unknown_root_without_spawning() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.project_registry.clear();
        app.boot_root = None;
        app.terminal_ui.error = None;
        let result = maintain(&mut app);
        assert!(result);
        assert!(app.terminal_ui.sessions.is_empty());
        assert!(app.terminal_ui.error.is_some());
    }

    #[test]
    fn relaunch_discards_the_session_and_error() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        app.terminal_ui.error = Some("boom".to_owned());
        relaunch(&mut app);
        assert!(app.terminal_ui.sessions.is_empty());
        assert!(app.terminal_ui.error.is_none());
    }

    #[test]
    fn a_live_shell_absorbs_every_key_including_quit_and_command() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        assert!(handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE
            )
        ));
        assert!(handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                KeyCode::Char(':'),
                crossterm::event::KeyModifiers::NONE
            )
        ));
        assert!(!app.should_quit());
        assert!(app.pending.is_empty());
    }

    #[test]
    fn scroll_offsets_the_parser_and_returns_to_bottom() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        if let Some(session) = app.terminal_ui.sessions.get("main") {
            let mut output = Vec::new();
            for line in 1..=30 {
                output.extend_from_slice(format!("line {line}\r\n").as_bytes());
            }
            session.feed(&output);
        }
        scroll(&mut app, true);
        scroll(&mut app, true);
        assert_eq!(app.terminal_ui.scroll_offset, 6);
        assert_eq!(app.terminal_ui.sessions["main"].scrollback(), 6);
        scroll(&mut app, false);
        assert_eq!(app.terminal_ui.scroll_offset, 3);
        assert_eq!(app.terminal_ui.sessions["main"].scrollback(), 3);
    }

    #[test]
    fn paste_goes_to_the_live_shell_and_resets_scroll() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        app.terminal_ui.scroll_offset = 9;
        paste(&mut app, "multi\nline");
        assert_eq!(app.terminal_ui.scroll_offset, 0);
    }

    #[test]
    fn snapshot_terminal_screen_with_fed_output() {
        let mut app = app_with_terminal("main");
        assert!(maintain(&mut app));
        if let Some(session) = app.terminal_ui.sessions.get("main") {
            session.feed(b"\x1b[1;31mcoducktor\x1b[0m ready\r\n\x1b[32m$\x1b[0m ");
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        insta::assert_debug_snapshot!("terminal_embedded_80x24", terminal.backend().buffer());
    }

    #[test]
    fn snapshot_terminal_screen_without_a_session() {
        let mut app = app_with_terminal("main");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        insta::assert_debug_snapshot!("terminal_starting_80x24", terminal.backend().buffer());
    }
}
