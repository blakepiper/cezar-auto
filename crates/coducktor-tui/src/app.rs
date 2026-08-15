use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::input::hitmap::{HitAction, HitMap};
use crate::input::keymap::{ActionId, KeyMode, Keymap};
use crate::theme::{Theme, ThemeName};

/// The two real placeholder screens used to prove the A3 router and shell plumbing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Tasks { project: String },
    GlobalTasks,
}

impl Route {
    pub fn parse(path: &str, default_project: &str) -> Option<Self> {
        let path = path.split(['?', '#']).next().unwrap_or(path);
        if path == "/" || path == "/tasks/current" {
            return Some(Self::Tasks {
                project: default_project.to_owned(),
            });
        }
        if path == "/tasks" {
            return Some(Self::GlobalTasks);
        }
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts.first() == Some(&"p") {
            let project = (*parts.get(1)?).to_owned();
            if parts.len() == 2 || (parts.len() == 3 && parts[2] == "tasks") {
                return Some(Self::Tasks { project });
            }
        }
        None
    }

    pub fn path(&self) -> String {
        match self {
            Self::Tasks { project } => format!("/p/{project}"),
            Self::GlobalTasks => "/tasks".to_owned(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Tasks { .. } => "TASKS",
            Self::GlobalTasks => "GLOBAL TASKS",
        }
    }
}

/// Browser-like back/forward history for terminal routes.
#[derive(Debug, Clone)]
pub struct History {
    current: Route,
    back: Vec<Route>,
    forward: Vec<Route>,
}

impl History {
    pub fn new(initial: Route) -> Self {
        Self {
            current: initial,
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    pub fn current(&self) -> &Route {
        &self.current
    }

    pub fn navigate(&mut self, route: Route) {
        if self.current == route {
            return;
        }
        let current = std::mem::replace(&mut self.current, route);
        self.back.push(current);
        self.forward.clear();
    }

    pub fn back(&mut self) -> bool {
        let Some(route) = self.back.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, route);
        self.forward.push(current);
        true
    }

    pub fn forward(&mut self) -> bool {
        let Some(route) = self.forward.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, route);
        self.back.push(current);
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Command,
}

/// A3's routed app shell. Later screens plug into this state without changing the event loop.
pub struct App {
    pub history: History,
    pub hitmap: HitMap,
    pub theme: Theme,
    keymap: Keymap,
    mode: InputMode,
    command: String,
    notice: Option<String>,
    hover: Option<(u16, u16)>,
    quit: bool,
}

impl App {
    pub fn new(project: impl Into<String>, theme: Theme, keymap: Keymap) -> Self {
        Self {
            history: History::new(Route::Tasks {
                project: project.into(),
            }),
            hitmap: HitMap::default(),
            theme,
            keymap,
            mode: InputMode::Normal,
            command: String::new(),
            notice: None,
            hover: None,
            quit: false,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn route(&self) -> &Route {
        self.history.current()
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => {
                self.hover = Some((mouse.column, mouse.row));
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && let Some(action) = self.hitmap.hit(mouse.column, mouse.row)
                {
                    self.apply_hit_action(action);
                }
            }
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        self.hitmap.clear();
        let area = frame.area();
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(frame, vertical[0]);
        self.render_body(frame, vertical[1]);
        self.render_status(frame, vertical[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let line = Line::from(vec![
            Span::styled(
                " coducktor ",
                Style::default()
                    .fg(self.theme.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("A3 shell", Style::default().fg(self.theme.palette.soft_fg)),
            Span::raw("  "),
            Span::styled(
                self.history.current().path(),
                Style::default().fg(self.theme.palette.fg),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(self.theme.palette.surface)),
            area,
        );
    }

    fn render_body(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 40 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(18), Constraint::Min(1)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1)])
                .split(area)
        };
        if let Some(sidebar) = columns.first().copied().filter(|_| columns.len() > 1) {
            self.render_sidebar(frame, sidebar);
            self.render_screen(frame, columns[1]);
        } else if let Some(content) = columns.first().copied() {
            self.render_screen(frame, content);
        }
    }

    fn render_sidebar(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(self.theme.palette.border))
            .style(Style::default().bg(self.theme.palette.surface));
        frame.render_widget(block, area);
        let lines = vec![
            Line::from(Span::styled(
                "  NAVIGATION",
                Style::default().fg(self.theme.palette.soft_fg),
            )),
            Line::from(Span::styled(
                "  Tasks",
                self.nav_style(matches!(self.route(), Route::Tasks { .. })),
            )),
            Line::from(Span::styled(
                "  Global tasks",
                self.nav_style(matches!(self.route(), Route::GlobalTasks)),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Placeholder screens",
                Style::default().fg(self.theme.palette.soft_fg),
            )),
        ];
        let content = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        frame.render_widget(content, Rect::new(area.x, area.y, area.width, area.height));
        self.hitmap.register(
            Rect::new(area.x, area.y + 1, area.width, 1),
            1,
            HitAction::Tasks,
        );
        self.hitmap.register(
            Rect::new(area.x, area.y + 2, area.width, 1),
            1,
            HitAction::GlobalTasks,
        );
    }

    fn render_screen(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let title = self.route().title();
        let body = match self.route() {
            Route::Tasks { project } => format!(
                "{title}\n\nProject: {project}\n\nThis placeholder proves keyboard, mouse, command, and history routing.\nPress g for global tasks, :open /tasks, or Esc to go back."
            ),
            Route::GlobalTasks => format!(
                "{title}\n\nAll registered projects\n\nThis placeholder is ready for the global task table in A5.\nPress t for tasks, :open /p/main, or Esc to go back."
            ),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(self.theme.palette.border));
        frame.render_widget(
            Paragraph::new(body)
                .block(block)
                .style(
                    Style::default()
                        .fg(self.theme.palette.fg)
                        .bg(self.theme.palette.bg),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
        if let Some((column, row)) = self.hover
            && area.contains((column, row).into())
        {
            self.hitmap.register(area, 0, HitAction::Tasks);
        }
    }

    fn render_status(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mode = match self.mode {
            InputMode::Normal => "NORMAL",
            InputMode::Command => "COMMAND",
        };
        let line = if self.mode == InputMode::Command {
            format!(" {mode} :{}", self.command)
        } else if let Some(notice) = &self.notice {
            format!(" {mode}  {notice}")
        } else {
            format!(
                " {mode}  {}  |  :open <route>  ? help",
                self.theme.name.label()
            )
        };
        frame.render_widget(
            Paragraph::new(line).style(
                Style::default()
                    .fg(self.theme.palette.soft_fg)
                    .bg(self.theme.palette.surface),
            ),
            area,
        );
        self.hitmap.register(
            Rect::new(area.x, area.y, area.width.min(8), area.height),
            2,
            HitAction::Back,
        );
        self.hitmap.register(
            Rect::new(
                area.x.saturating_add(8),
                area.y,
                area.width.saturating_sub(8).min(10),
                area.height,
            ),
            2,
            HitAction::Forward,
        );
        self.hitmap.register(
            Rect::new(
                area.right().saturating_sub(3),
                area.y,
                area.width.min(3),
                area.height,
            ),
            2,
            HitAction::Quit,
        );
    }

    fn nav_style(&self, active: bool) -> Style {
        if active {
            Style::default()
                .fg(self.theme.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.palette.fg)
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.mode == InputMode::Command {
            self.handle_command_key(key);
            return;
        }
        if let Some(action) = self.keymap.action_for(KeyMode::Normal, &key) {
            self.apply_action(action);
            return;
        }
        match key.code {
            KeyCode::Char(':') => {
                self.mode = InputMode::Command;
                self.command.clear();
                self.notice = None;
            }
            KeyCode::Esc => {
                self.history.back();
            }
            _ => {}
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.command.clear();
            }
            KeyCode::Enter => {
                let command = std::mem::take(&mut self.command);
                self.mode = InputMode::Normal;
                self.execute_command(&command);
            }
            KeyCode::Backspace => {
                self.command.pop();
            }
            KeyCode::Char(character) => self.command.push(character),
            _ => {}
        }
    }

    fn execute_command(&mut self, command: &str) {
        let mut parts = command.split_whitespace();
        match parts.next() {
            Some("open") => {
                if let Some(path) = parts.next() {
                    match Route::parse(path, "main") {
                        Some(route) => self.history.navigate(route),
                        None => self.notice = Some(format!("unknown route: {path}")),
                    }
                } else {
                    self.notice = Some("usage: :open <route>".to_owned());
                }
            }
            Some("back") => {
                self.history.back();
            }
            Some("forward") => {
                self.history.forward();
            }
            Some("theme") => {
                if let Some(name) = parts.next().and_then(ThemeName::parse) {
                    self.theme = Theme::new(name, self.theme.capability);
                } else {
                    self.notice = Some("theme must be light, dark, or lazyvim".to_owned());
                }
            }
            Some("quit") => self.quit = true,
            Some(unknown) => self.notice = Some(format!("unknown command: {unknown}")),
            None => {}
        }
    }

    fn apply_action(&mut self, action: ActionId) {
        match action {
            ActionId::Quit => self.quit = true,
            ActionId::Tasks => self.history.navigate(Route::Tasks {
                project: "main".to_owned(),
            }),
            ActionId::GlobalTasks => self.history.navigate(Route::GlobalTasks),
            ActionId::Back => {
                self.history.back();
            }
            ActionId::Forward => {
                self.history.forward();
            }
            ActionId::Command => {
                self.mode = InputMode::Command;
                self.command.clear();
            }
            ActionId::ExecuteCommand | ActionId::Normal | ActionId::Noop => {}
        }
    }

    fn apply_hit_action(&mut self, action: HitAction) {
        match action {
            HitAction::Tasks => self.apply_action(ActionId::Tasks),
            HitAction::GlobalTasks => self.apply_action(ActionId::GlobalTasks),
            HitAction::Back => self.apply_action(ActionId::Back),
            HitAction::Forward => self.apply_action(ActionId::Forward),
            HitAction::Quit => self.apply_action(ActionId::Quit),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn route_history_supports_back_and_forward() {
        let initial = Route::Tasks {
            project: "main".to_owned(),
        };
        let mut history = History::new(initial.clone());
        history.navigate(Route::GlobalTasks);
        assert_eq!(history.current(), &Route::GlobalTasks);
        assert!(history.back());
        assert_eq!(history.current(), &initial);
        assert!(history.forward());
        assert_eq!(history.current(), &Route::GlobalTasks);
    }

    #[test]
    fn command_open_and_mouse_navigation_change_routes() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        for key in ":open /tasks".chars() {
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )));
        }
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.route(), &Route::GlobalTasks);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(app.route(), Route::Tasks { .. }));
    }

    #[test]
    fn key_navigation_and_history_shortcuts_change_routes() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.route(), &Route::GlobalTasks);
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(app.route(), Route::Tasks { .. }));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.route(), &Route::GlobalTasks);
    }

    #[test]
    fn renders_at_the_three_a3_snapshot_sizes() {
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::new("main", Theme::detect(), Keymap::default());
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("tasks_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
