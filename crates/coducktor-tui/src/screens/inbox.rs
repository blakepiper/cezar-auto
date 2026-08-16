//! The Inbox screen (spec §8.12) — replaces `routes/inbox.tsx`. The follow-up list an agent
//! leaves for a human: `GET /todos`, `DELETE /todos/:id` (dismiss), `POST /todos/:id/start`
//! (▶ run). Gated on `capabilities.followups`: when the capability is off the route returns
//! an empty list, so this screen asks `health` itself and renders the opt-in explainer —
//! "Inbox empty" would be a lie for a feature that is switched off.

use coducktor_contract::TodoItem;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, Route};

/// Engine-fetched state for the open Inbox screen.
#[derive(Default)]
pub struct InboxUi {
    pub project: String,
    pub todos: Option<Vec<TodoItem>>,
    pub selected: usize,
    /// `None` until health answered; distinguishes "inbox off" from "still loading".
    pub followups_enabled: Option<bool>,
}

/// The legacy `visibleTodos()` rule: started entries are the audit trail, not the inbox.
pub fn visible_todos(todos: &[TodoItem]) -> Vec<TodoItem> {
    todos
        .iter()
        .filter(|todo| todo.started_task_id.is_none())
        .cloned()
        .collect()
}

pub fn open(app: &mut App, project: &str) {
    if app.inbox_ui.project != project {
        app.inbox_ui = InboxUi {
            project: project.to_owned(),
            ..InboxUi::default()
        };
    }
    app.request_navigate(Route::Inbox {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadInbox {
        project: project.to_owned(),
    });
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(enabled) = app.inbox_ui.followups_enabled else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    if !enabled {
        render_disabled_explainer(frame, area, app);
        return;
    }
    let todos = app
        .inbox_ui
        .todos
        .clone()
        .map(|todos| visible_todos(&todos));
    match todos {
        None => {
            frame.render_widget(
                Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
                area,
            );
        }
        Some(todos) if todos.is_empty() => {
            frame.render_widget(
                Paragraph::new(
                    "Nothing waiting on you.\n\nFollow-ups agents leave behind land here — \
                     run or dismiss them with Enter / x.",
                )
                .style(Style::default().fg(app.theme.palette.soft_fg)),
                area,
            );
        }
        Some(todos) => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("{} waiting — Enter run · x dismiss · Esc back", todos.len()),
                    Style::default().fg(app.theme.palette.soft_fg),
                ))),
                rows[0],
            );
            let selected = app.inbox_ui.selected.min(todos.len().saturating_sub(1));
            let block = Block::default().borders(Borders::ALL).title("Inbox");
            let inner = block.inner(rows[1]);
            frame.render_widget(block, rows[1]);
            let lines: Vec<Line<'static>> = todos
                .iter()
                .enumerate()
                .take(inner.height as usize)
                .map(|(index, todo)| todo_line(todo, &app.theme, index == selected))
                .collect();
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            for (index, _) in todos.iter().enumerate().take(inner.height as usize) {
                if let Some(y) = inner.y.checked_add(index as u16)
                    && y < inner.bottom()
                {
                    app.hitmap.register(
                        Rect::new(inner.x, y, inner.width, 1),
                        2,
                        crate::input::hitmap::HitAction::InboxSelect(index),
                    );
                }
            }
            app.inbox_ui.selected = selected;
        }
    }
}

fn render_disabled_explainer(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let lines = vec![
        Line::from(Span::styled(
            "The follow-up inbox is off",
            Style::default()
                .fg(app.theme.palette.fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Agents only leave follow-ups here when the DUCK_FOLLOWUPS flag is set when the \
             service starts.",
            Style::default().fg(app.theme.palette.soft_fg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Restart the service with DUCK_FOLLOWUPS=1 to enable the inbox.",
            Style::default().fg(app.theme.palette.accent),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn todo_line(todo: &TodoItem, theme: &crate::theme::Theme, selected: bool) -> Line<'static> {
    let mut style = Style::default().fg(theme.palette.fg);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let mut spans = Vec::new();
    if let Some(ts) = &todo.ts {
        spans.push(Span::styled(
            format!("{}  ", short_age(ts)),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    spans.push(Span::styled(todo.summary.clone(), style));
    if let Some(skill) = &todo.suggested_skill {
        spans.push(Span::styled(
            format!("  [{}]", skill),
            Style::default().fg(theme.palette.accent),
        ));
    }
    if let Some(pr_url) = &todo.pr_url {
        spans.push(Span::styled(
            format!("  PR {}", pr_url),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    Line::from(spans)
}

/// The wire `ts` is RFC 3339; the TUI has no time dependency (spec §6.2 picked none yet),
/// so the entry shows its UTC date — enough to tell "yesterday" from "three weeks ago"
/// without a datetime crate, and the summary carries the substance.
fn short_age(ts: &str) -> String {
    ts.chars().take(10).collect()
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.inbox_ui.followups_enabled != Some(true) {
        return key.code == KeyCode::Esc && {
            app.request_back();
            true
        };
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let count = app
                .inbox_ui
                .todos
                .as_ref()
                .map(|todos| visible_todos(todos).len())
                .unwrap_or(0);
            if count > 0 {
                app.inbox_ui.selected = (app.inbox_ui.selected + 1).min(count - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.inbox_ui.selected = app.inbox_ui.selected.saturating_sub(1);
            true
        }
        KeyCode::Enter => {
            let Some(todo) = visible_todos(app.inbox_ui.todos.as_deref().unwrap_or_default())
                .into_iter()
                .nth(app.inbox_ui.selected)
            else {
                return true;
            };
            let project = app.inbox_ui.project.clone();
            app.pending.push(PendingAction::StartTodo {
                project,
                id: todo.id,
            });
            true
        }
        KeyCode::Char('x') | KeyCode::Delete => {
            let Some(todo) = visible_todos(app.inbox_ui.todos.as_deref().unwrap_or_default())
                .into_iter()
                .nth(app.inbox_ui.selected)
            else {
                return true;
            };
            let project = app.inbox_ui.project.clone();
            app.pending.push(PendingAction::DismissTodo {
                project,
                id: todo.id,
            });
            true
        }
        KeyCode::Esc => {
            app.request_back();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn todo(id: &str, summary: &str, started: bool) -> TodoItem {
        TodoItem {
            id: id.to_owned(),
            ts: Some("2026-08-10T00:00:00Z".to_owned()),
            task_id: None,
            summary: summary.to_owned(),
            action: None,
            pr_url: None,
            suggested_skill: Some("om-fix".to_owned()),
            suggested_args: None,
            suggested_prompt: None,
            runnable: Some(true),
            started_task_id: started.then(|| "run-9".to_owned()),
        }
    }

    fn app_with_inbox(enabled: bool) -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.inbox_ui.followups_enabled = Some(enabled);
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn followups_off_renders_the_opt_in_explainer() {
        let mut app = app_with_inbox(false);
        app.inbox_ui.todos = Some(vec![todo("t1", "Review the PR", false)]);
        let content = render(&mut app, 120, 40);
        assert!(content.contains("The follow-up inbox is off"));
        assert!(content.contains("DUCK_FOLLOWUPS"));
        assert!(
            !content.contains("Review the PR"),
            "off inbox shows no entries"
        );
    }

    #[test]
    fn entries_render_with_suggested_skills_and_started_ones_are_hidden() {
        let mut app = app_with_inbox(true);
        app.inbox_ui.todos = Some(vec![
            todo("t1", "Review the PR", false),
            todo("t2", "Already running", true),
        ]);
        let content = render(&mut app, 120, 40);
        assert!(content.contains("Review the PR"));
        assert!(content.contains("om-fix"));
        assert!(!content.contains("Already running"));
        assert_eq!(visible_todos(&app.inbox_ui.todos.clone().unwrap()).len(), 1);
    }

    #[test]
    fn enter_starts_and_x_dismisses() {
        let mut app = app_with_inbox(true);
        app.inbox_ui.todos = Some(vec![todo("t1", "Review the PR", false)]);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.pending
                .iter()
                .any(|action| matches!(action, PendingAction::StartTodo { id, .. } if id == "t1"))
        );
        app.pending.clear();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(
            app.pending.iter().any(
                |action| matches!(action, PendingAction::DismissTodo { id, .. } if id == "t1")
            )
        );
    }

    #[test]
    fn snapshot_inbox_at_three_sizes() {
        let mut app = app_with_inbox(true);
        app.inbox_ui.todos = Some(vec![
            todo("t1", "Review the pull request", false),
            todo("t2", "Decide the follow-up question", false),
        ]);
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("inbox_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
