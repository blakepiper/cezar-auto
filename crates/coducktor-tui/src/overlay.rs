//! The command palette — `Ctrl+K`, `nucleo`-scored, over Tasks
//! (cross-project, from `/workspace/runs-index`), Views, Projects and Actions. Every entry
//! shares a stable action with `:` command-line equivalents where one exists (`execute_command`
//! is `pub(crate)` for exactly this reuse) and the keymap.

use nucleo::{Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, NavItem};

#[derive(Clone)]
enum PaletteAction {
    Nav(NavItem),
    GlobalSettings,
    Command(&'static str),
    OpenTask { project: String, id: String },
    SwitchProject(String),
}

#[derive(Clone)]
struct Entry {
    group: &'static str,
    label: String,
    action: PaletteAction,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    matcher: Matcher,
}

const VIEWS: [(NavItem, &str); 10] = [
    (NavItem::NewTask, "New task"),
    (NavItem::Tasks, "Tasks"),
    (NavItem::Scratchpad, "Scratchpad"),
    (NavItem::Ide, "IDE"),
    (NavItem::Terminal, "Terminal"),
    (NavItem::RepoGit, "Repo git"),
    (NavItem::Github, "GitHub"),
    (NavItem::Skills, "Skills"),
    (NavItem::Workflows, "Workflows"),
    (NavItem::Settings, "Settings"),
];

const ACTIONS: [(&str, &str); 5] = [
    ("Toggle sidebar", "sidebar"),
    ("Help", "help"),
    ("Back", "back"),
    ("Forward", "forward"),
    ("Quit", "quit"),
];

fn candidates(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(index) = &app.global_index {
        for run in &index.runs {
            entries.push(Entry {
                group: "Tasks",
                label: format!("{}  ({})", run.title, run.project_id),
                action: PaletteAction::OpenTask {
                    project: run.project_id.clone(),
                    id: run.id.clone(),
                },
            });
        }
    }
    for (nav, label) in VIEWS {
        entries.push(Entry {
            group: "Views",
            label: label.to_owned(),
            action: PaletteAction::Nav(nav),
        });
    }
    entries.push(Entry {
        group: "Views",
        label: "Global settings".to_owned(),
        action: PaletteAction::GlobalSettings,
    });
    for project in &app.projects {
        entries.push(Entry {
            group: "Projects",
            label: project.name.clone(),
            action: PaletteAction::SwitchProject(project.id.clone()),
        });
    }
    for (label, command) in ACTIONS {
        entries.push(Entry {
            group: "Actions",
            label: label.to_owned(),
            action: PaletteAction::Command(command),
        });
    }
    entries
}

fn score(matcher: &mut Matcher, haystack: &str, needle: &str) -> Option<u32> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut hay_buf = Vec::new();
    let mut needle_buf = Vec::new();
    let hay = Utf32Str::new(haystack, &mut hay_buf);
    let needle = Utf32Str::new(needle, &mut needle_buf);
    matcher.fuzzy_match(hay, needle).map(u32::from)
}

fn ranked(app: &mut App) -> Vec<Entry> {
    let query = app.palette.query.clone();
    let candidates = candidates(app);
    let matcher = &mut app.palette.matcher;
    let mut scored: Vec<(u32, Entry)> = candidates
        .into_iter()
        .filter_map(|entry| score(matcher, &entry.label, &query).map(|s| (s, entry)))
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

pub fn open(app: &mut App) {
    app.palette.open = true;
    app.palette.query.clear();
    app.palette.selected = 0;
}

pub fn close(app: &mut App) {
    app.palette.open = false;
}

pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Esc => close(app),
        KeyCode::Down => {
            let len = ranked(app).len();
            app.palette.selected = (app.palette.selected + 1).min(len.saturating_sub(1));
        }
        KeyCode::Up => {
            app.palette.selected = app.palette.selected.saturating_sub(1);
        }
        KeyCode::Enter => {
            let entries = ranked(app);
            if let Some(entry) = entries.get(app.palette.selected).cloned() {
                close(app);
                activate(app, entry.action);
            }
        }
        KeyCode::Backspace => {
            app.palette.query.pop();
            app.palette.selected = 0;
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.palette.query.push(character);
            app.palette.selected = 0;
        }
        _ => {}
    }
    true
}

fn activate(app: &mut App, action: PaletteAction) {
    match action {
        PaletteAction::Nav(nav) => app.navigate(nav),
        PaletteAction::GlobalSettings => crate::screens::settings::open_global(app),
        PaletteAction::Command(command) => app.execute_command(command),
        PaletteAction::OpenTask { project, id } => crate::screens::thread::open(app, &project, &id),
        PaletteAction::SwitchProject(project) => app.switch_project(project),
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let width = area.width.saturating_sub(area.width / 4).clamp(40, 100);
    let height = area.height.saturating_sub(area.height / 4).clamp(10, 24);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 4;
    let popup = Rect::new(
        x,
        y,
        width,
        height.min(area.height.saturating_sub(y - area.y)),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("⌘K  {}_", app.palette.query));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let entries = ranked(app);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_group = "";
    for (index, entry) in entries.iter().enumerate().take(inner.height as usize) {
        if entry.group != current_group {
            current_group = entry.group;
            lines.push(Line::from(Span::styled(
                current_group.to_uppercase(),
                Style::default().fg(app.theme.palette.soft_fg),
            )));
        }
        let mut style = Style::default().fg(app.theme.palette.fg);
        if index == app.palette.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(format!(" {}", entry.label), style)));
    }
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matches.",
            Style::default().fg(app.theme.palette.soft_fg),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use crate::app::Route;

    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn opening_lists_views_and_actions() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app);
        let entries = ranked(&mut app);
        assert!(entries.iter().any(|entry| entry.label == "Settings"));
        assert!(entries.iter().any(|entry| entry.label == "Global settings"));
        assert!(entries.iter().any(|entry| entry.label == "Quit"));
    }

    #[test]
    fn typing_filters_by_fuzzy_score() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app);
        for character in "sett".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        let entries = ranked(&mut app);
        assert!(!entries.is_empty());
        assert_eq!(entries[0].label, "Settings");
    }

    #[test]
    fn enter_on_settings_navigates_there() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app);
        for character in "settings".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.palette.open);
        assert!(matches!(app.route(), Route::Settings { .. }));
    }

    #[test]
    fn enter_on_global_settings_navigates_to_the_workspace_route() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app);
        for character in "global settings".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.palette.open);
        assert_eq!(app.route(), &Route::GlobalSettings);
    }

    #[test]
    fn renders_without_panicking() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut app))
            .unwrap();
    }
}
