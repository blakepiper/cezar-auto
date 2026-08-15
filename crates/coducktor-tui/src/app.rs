use std::collections::BTreeMap;

use coducktor_contract::{ApiRun, RunStatus};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::input::hitmap::{HitAction, HitMap};
use crate::input::keymap::{ActionId, KeyMode, Keymap};
use crate::service::ServiceState;
use crate::theme::{Theme, ThemeName};

const SIDEBAR_BREAKPOINT: u16 = 100;
const SIDEBAR_DEFAULT_WIDTH: u16 = 28;
const SIDEBAR_MIN_WIDTH: u16 = 20;
const SIDEBAR_MAX_WIDTH: u16 = 44;

/// Shell navigation targets. Content screens replace the placeholder route as later plan steps land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    NewTask,
    Tasks,
    Inbox,
    Ide,
    RepoGit,
    Github,
    Skills,
    Workflows,
    Settings,
}

impl NavItem {
    const ALL: [Self; 9] = [
        Self::NewTask,
        Self::Tasks,
        Self::Inbox,
        Self::Ide,
        Self::RepoGit,
        Self::Github,
        Self::Skills,
        Self::Workflows,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NewTask => "New task",
            Self::Tasks => "Tasks",
            Self::Inbox => "Inbox",
            Self::Ide => "IDE",
            Self::RepoGit => "Git",
            Self::Github => "GitHub",
            Self::Skills => "Skills",
            Self::Workflows => "Workflows",
            Self::Settings => "Settings",
        }
    }

    fn path_segment(self) -> &'static str {
        match self {
            Self::NewTask => "new",
            Self::Tasks => "tasks",
            Self::Inbox => "inbox",
            Self::Ide => "ide",
            Self::RepoGit => "repo-git",
            Self::Github => "github",
            Self::Skills => "skills",
            Self::Workflows => "workflows",
            Self::Settings => "settings",
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        match segment {
            "new" | "new-task" => Some(Self::NewTask),
            "tasks" => Some(Self::Tasks),
            "inbox" => Some(Self::Inbox),
            "ide" => Some(Self::Ide),
            "git" | "repo-git" => Some(Self::RepoGit),
            "github" => Some(Self::Github),
            "skills" => Some(Self::Skills),
            "workflows" => Some(Self::Workflows),
            "settings" => Some(Self::Settings),
            _ => None,
        }
    }
}

/// The routed identity used by the TUI. Later screens keep this URL-shaped seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Tasks { project: String },
    GlobalTasks,
    Placeholder { project: String, nav: NavItem },
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
            return match parts.get(2).copied() {
                None | Some("tasks") => Some(Self::Tasks { project }),
                Some(segment) => {
                    NavItem::parse(segment).map(|nav| Self::Placeholder { project, nav })
                }
            };
        }
        None
    }

    pub fn path(&self) -> String {
        match self {
            Self::Tasks { project } => format!("/p/{project}"),
            Self::GlobalTasks => "/tasks".to_owned(),
            Self::Placeholder { project, nav } => {
                format!("/p/{project}/{}", nav.path_segment())
            }
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Tasks { .. } => "TASKS",
            Self::GlobalTasks => "GLOBAL TASKS",
            Self::Placeholder { nav, .. } => nav.uppercase_title(),
        }
    }

    fn project(&self) -> Option<&str> {
        match self {
            Self::Tasks { project } | Self::Placeholder { project, .. } => Some(project),
            Self::GlobalTasks => None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskFilter {
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskGroup {
    NeedsYou,
    Working,
    Recent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTask {
    pub project: String,
    pub id: String,
    pub title: String,
    pub status: RunStatus,
    pub archived: bool,
    pub unread: bool,
    pub created_at: String,
}

impl QuickTask {
    pub fn from_api(project: impl Into<String>, run: ApiRun) -> Self {
        let record = run.record;
        Self {
            project: project.into(),
            id: record.id,
            title: record.title,
            status: record.status,
            archived: record.archived,
            unread: record.seen_at.is_none(),
            created_at: record.created_at,
        }
    }

    fn group(&self) -> TaskGroup {
        match self.status {
            RunStatus::Queued | RunStatus::Running => TaskGroup::Working,
            RunStatus::Waiting | RunStatus::Review => TaskGroup::NeedsYou,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => {
                if self.unread {
                    TaskGroup::NeedsYou
                } else {
                    TaskGroup::Recent
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEvent {
    Run(QuickTask),
    RunDeleted { project: String, id: String },
    Todos { project: String, count: usize },
    ProviderStatus { provider: String, available: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderBadge {
    name: String,
    available: bool,
}

/// The A4 shell state. Content screens can consume its navigation and live summaries later.
pub struct App {
    pub history: History,
    pub hitmap: HitMap,
    pub theme: Theme,
    keymap: Keymap,
    mode: InputMode,
    command: String,
    notice: Option<String>,
    toast: Option<String>,
    hover: Option<(u16, u16)>,
    quit: bool,
    confirm_quit: bool,
    help_open: bool,
    default_project: String,
    projects: Vec<ProjectEntry>,
    quick_tasks: Vec<QuickTask>,
    todo_counts: BTreeMap<String, usize>,
    task_filter: TaskFilter,
    sidebar_width: u16,
    sidebar_collapsed: bool,
    sidebar_overlay_open: bool,
    sidebar_dragging: bool,
    last_width: u16,
    service_state: ServiceState,
    providers: Vec<ProviderBadge>,
}

impl App {
    pub fn new(project: impl Into<String>, theme: Theme, keymap: Keymap) -> Self {
        let project = project.into();
        Self {
            history: History::new(Route::Tasks {
                project: project.clone(),
            }),
            hitmap: HitMap::default(),
            theme,
            keymap,
            mode: InputMode::Normal,
            command: String::new(),
            notice: None,
            toast: None,
            hover: None,
            quit: false,
            confirm_quit: false,
            help_open: false,
            default_project: project.clone(),
            projects: vec![ProjectEntry {
                id: project.clone(),
                name: project,
                collapsed: false,
            }],
            quick_tasks: Vec::new(),
            todo_counts: BTreeMap::new(),
            task_filter: TaskFilter::Active,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            sidebar_collapsed: false,
            sidebar_overlay_open: false,
            sidebar_dragging: false,
            last_width: 0,
            service_state: ServiceState::Disabled,
            providers: Vec::new(),
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn route(&self) -> &Route {
        self.history.current()
    }

    pub fn current_project(&self) -> &str {
        self.route()
            .project()
            .unwrap_or(self.default_project.as_str())
    }

    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = (String, String)>) {
        self.projects = projects
            .into_iter()
            .map(|(id, name)| ProjectEntry {
                id,
                name,
                collapsed: false,
            })
            .collect();
        if self.projects.is_empty() {
            self.projects.push(ProjectEntry {
                id: self.default_project.clone(),
                name: self.default_project.clone(),
                collapsed: false,
            });
        }
    }

    pub fn set_quick_tasks(&mut self, tasks: impl IntoIterator<Item = QuickTask>) {
        self.quick_tasks = tasks.into_iter().collect();
    }

    pub fn set_service_state(&mut self, state: ServiceState) {
        self.service_state = state;
    }

    pub fn set_provider_states(&mut self, states: impl IntoIterator<Item = (String, bool)>) {
        self.providers = states
            .into_iter()
            .map(|(name, available)| ProviderBadge { name, available })
            .collect();
    }

    pub fn apply_workspace_event(&mut self, event: WorkspaceEvent) {
        match event {
            WorkspaceEvent::Run(task) => {
                if let Some(existing) = self
                    .quick_tasks
                    .iter_mut()
                    .find(|existing| existing.project == task.project && existing.id == task.id)
                {
                    *existing = task;
                } else {
                    self.quick_tasks.push(task);
                }
            }
            WorkspaceEvent::RunDeleted { project, id } => {
                self.quick_tasks
                    .retain(|task| task.project != project || task.id != id);
            }
            WorkspaceEvent::Todos { project, count } => {
                self.todo_counts.insert(project, count);
            }
            WorkspaceEvent::ProviderStatus {
                provider,
                available,
            } => {
                if let Some(existing) = self
                    .providers
                    .iter_mut()
                    .find(|existing| existing.name == provider)
                {
                    existing.available = available;
                } else {
                    self.providers.push(ProviderBadge {
                        name: provider,
                        available,
                    });
                }
            }
        }
    }

    pub fn running_count(&self) -> usize {
        self.quick_tasks
            .iter()
            .filter(|task| {
                !task.archived && matches!(task.status, RunStatus::Queued | RunStatus::Running)
            })
            .count()
    }

    pub fn needs_you_count(&self) -> usize {
        self.quick_tasks
            .iter()
            .filter(|task| !task.archived && task.group() == TaskGroup::NeedsYou)
            .count()
    }

    pub fn inbox_count(&self) -> usize {
        self.todo_counts.values().sum()
    }

    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width
    }

    pub fn sidebar_is_visible(&self, width: u16) -> bool {
        (width >= SIDEBAR_BREAKPOINT && !self.sidebar_collapsed)
            || (width < SIDEBAR_BREAKPOINT && self.sidebar_overlay_open)
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        self.hitmap.clear();
        let area = frame.area();
        self.last_width = area.width;
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
        self.render_overlays(frame, area);
    }

    fn render_header(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let project = self.current_project().to_owned();
        let route = self.route().path();
        let line = Line::from(vec![
            Span::styled(
                " [=] coducktor ",
                Style::default()
                    .fg(self.theme.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("/ {project} {route}"),
                Style::default().fg(self.theme.palette.fg),
            ),
            Span::raw("  "),
            Span::styled(
                format!("[running {}]", self.running_count()),
                Style::default().fg(self.theme.palette.running),
            ),
            Span::raw(" "),
            Span::styled(
                format!("[needs {}]", self.needs_you_count()),
                Style::default().fg(self.theme.palette.waiting),
            ),
            Span::raw("  [Ctrl+K]"),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(self.theme.palette.surface)),
            area,
        );
        if area.width > 0 {
            self.hitmap.register(
                Rect::new(area.x, area.y, area.width.min(5), area.height),
                3,
                HitAction::ToggleSidebar,
            );
            self.hitmap.register(
                Rect::new(
                    area.right().saturating_sub(9),
                    area.y,
                    area.width.min(9),
                    area.height,
                ),
                3,
                HitAction::Help,
            );
        }
    }

    fn render_body(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if self.sidebar_is_visible(area.width) {
            let width = self
                .sidebar_width()
                .min(area.width.saturating_sub(24).max(SIDEBAR_MIN_WIDTH));
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(width), Constraint::Min(1)])
                .split(area);
            self.render_sidebar(frame, columns[0]);
            self.render_screen(frame, columns[1]);
        } else {
            self.render_screen(frame, area);
        }
    }

    fn render_sidebar(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mut rows: Vec<(Line<'static>, Option<HitAction>)> = Vec::new();
        rows.push((sidebar_line("  PROJECTS", self.soft_style()), None));
        for project in &self.projects {
            let marker = if project.collapsed { "+" } else { "-" };
            rows.push((
                sidebar_line(
                    format!("  {marker} {}", truncate(&project.name, 20)),
                    if project.id == self.current_project() {
                        self.active_style()
                    } else {
                        self.normal_style()
                    },
                ),
                Some(HitAction::ProjectToggle(project.id.clone())),
            ));
            if project.id == self.current_project() && !project.collapsed {
                rows.push((
                    sidebar_nav_line(
                        "New task",
                        None,
                        self.route_is(NavItem::NewTask),
                        self.nav_style(self.route_is(NavItem::NewTask)),
                    ),
                    Some(HitAction::NewTask),
                ));
                rows.push((
                    sidebar_nav_line(
                        "Tasks",
                        None,
                        self.route_is(NavItem::Tasks),
                        self.nav_style(self.route_is(NavItem::Tasks)),
                    ),
                    Some(HitAction::Tasks),
                ));
                rows.push((
                    sidebar_nav_line(
                        "Inbox",
                        Some(self.inbox_count()),
                        self.route_is(NavItem::Inbox),
                        self.nav_style(self.route_is(NavItem::Inbox)),
                    ),
                    Some(HitAction::Inbox),
                ));
                for nav in NavItem::ALL.into_iter().skip(3) {
                    rows.push((
                        sidebar_nav_line(
                            nav.label(),
                            None,
                            self.route_is(nav),
                            self.nav_style(self.route_is(nav)),
                        ),
                        Some(nav_hit_action(nav)),
                    ));
                }
            }
        }
        rows.push((sidebar_line("", self.soft_style()), None));
        rows.push((sidebar_line("  WORKSPACE", self.soft_style()), None));
        rows.push((
            sidebar_nav_line(
                "All tasks",
                None,
                matches!(self.route(), Route::GlobalTasks),
                self.nav_style(matches!(self.route(), Route::GlobalTasks)),
            ),
            Some(HitAction::GlobalTasks),
        ));
        rows.push((sidebar_line("", self.soft_style()), None));
        rows.push((sidebar_line("  TASKS", self.soft_style()), None));
        rows.push((
            sidebar_nav_line(
                "Active",
                None,
                self.task_filter == TaskFilter::Active,
                self.nav_style(self.task_filter == TaskFilter::Active),
            ),
            Some(HitAction::ActiveTasks),
        ));
        rows.push((
            sidebar_nav_line(
                "Archived",
                None,
                self.task_filter == TaskFilter::Archived,
                self.nav_style(self.task_filter == TaskFilter::Archived),
            ),
            Some(HitAction::ArchivedTasks),
        ));
        for group in [TaskGroup::NeedsYou, TaskGroup::Working, TaskGroup::Recent] {
            rows.push((sidebar_line(group.label(), self.soft_style()), None));
            for (displayed, task) in self
                .quick_tasks
                .iter()
                .filter(|task| {
                    task.archived == (self.task_filter == TaskFilter::Archived)
                        && task.group() == group
                })
                .enumerate()
            {
                if displayed == 2 {
                    break;
                }
                rows.push((
                    task_line(task, self.theme.palette_for_status(task.status)),
                    Some(HitAction::Tasks),
                ));
            }
        }

        let lines: Vec<Line<'static>> = rows.iter().map(|(line, _)| line.clone()).collect();
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .style(Style::default().bg(self.theme.palette.surface))
                .wrap(Wrap { trim: false }),
            area,
        );
        for (offset, (_, action)) in rows.into_iter().enumerate() {
            let Some(action) = action else {
                continue;
            };
            let Some(row) = area.y.checked_add(offset as u16) else {
                continue;
            };
            if row < area.bottom() {
                self.hitmap.register(
                    Rect::new(area.x, row, area.width.saturating_sub(1), 1),
                    2,
                    action,
                );
            }
        }
        if area.width > 0 {
            self.hitmap.register(
                Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
                10,
                HitAction::SidebarEdge,
            );
        }
    }

    fn render_screen(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let route = self.route().clone();
        let title = route.title();
        let body = match route {
            Route::Tasks { project } => format!(
                "{title}\n\nProject: {project}\n\nThis placeholder proves keyboard, mouse, command, and history routing.\nPress g for global tasks, :open /tasks, or Esc to go back."
            ),
            Route::GlobalTasks => format!(
                "{title}\n\nAll registered projects\n\nThis placeholder is ready for the global task table in A5.\nPress t for tasks, :open /p/main, or Esc to go back."
            ),
            Route::Placeholder { nav, project } => format!(
                "{title}\n\nProject: {project}\n\nThe shell route for {} is ready for its content screen in a later step.",
                nav.label()
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
        if area.width > 0 && area.height > 0 {
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
        } else if let Some(toast) = &self.toast {
            format!(" {mode}  {toast}")
        } else if let Some(notice) = &self.notice {
            format!(" {mode}  {notice}")
        } else {
            format!(
                " {mode}  {}  {}  v0.1.0  [server:{}]  {}  ? help",
                self.current_project(),
                self.theme.name.label(),
                service_state_label(self.service_state),
                self.provider_summary(),
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
        if area.width > 0 {
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
                Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
                2,
                HitAction::Quit,
            );
        }
    }

    fn render_overlays(&self, frame: &mut Frame<'_>, area: Rect) {
        if let Some(toast) = &self.toast {
            let width = (toast.len() as u16 + 4).min(area.width.saturating_sub(2));
            let rect = Rect::new(
                area.right().saturating_sub(width + 1),
                area.bottom().saturating_sub(3),
                width,
                3.min(area.height),
            );
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(toast.as_str())
                    .block(Block::default().borders(Borders::ALL).title("NOTICE"))
                    .style(Style::default().fg(self.theme.palette.fg)),
                rect,
            );
        }
        if self.help_open {
            self.render_help(frame, area);
        } else if self.confirm_quit {
            self.render_confirm(frame, area);
        }
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled(
                "NORMAL",
                Style::default().fg(self.theme.palette.accent),
            )),
            Line::from("  Mouse capture: F12 toggles it; hold Shift for terminal selection."),
            Line::from("  y copies the focused item; Esc closes this help."),
            Line::from(""),
        ];
        for (key, action) in self.keymap.help_bindings(KeyMode::Normal) {
            lines.push(Line::from(format!("  {key:<12} {action:?}")));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
        let width = area.width.min(72);
        let rect = centered_rect(area, width, height);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title("HELP"))
                .style(Style::default().fg(self.theme.palette.fg))
                .wrap(Wrap { trim: false }),
            rect,
        );
    }

    fn render_confirm(&self, frame: &mut Frame<'_>, area: Rect) {
        let rect = centered_rect(area, 48.min(area.width), 7.min(area.height));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new("Live tasks are still running. Quit anyway?\n\n  [y] yes    [n] no")
                .block(Block::default().borders(Borders::ALL).title("CONFIRM"))
                .style(Style::default().fg(self.theme.palette.fg))
                .wrap(Wrap { trim: false }),
            rect,
        );
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        self.hover = Some((mouse.column, mouse.row));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = self.hitmap.hit(mouse.column, mouse.row) {
                    if action == HitAction::SidebarEdge {
                        self.sidebar_dragging = true;
                    } else {
                        self.apply_hit_action(action);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sidebar_dragging => {
                self.sidebar_width = mouse
                    .column
                    .saturating_add(1)
                    .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
            }
            MouseEventKind::Up(MouseButton::Left) => self.sidebar_dragging = false,
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help_open = false;
            }
            return;
        }
        if self.confirm_quit {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.confirm_quit = false;
                    self.quit = true;
                }
                KeyCode::Char('n') | KeyCode::Esc => self.confirm_quit = false,
                _ => {}
            }
            return;
        }
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
            KeyCode::Char('?') => self.help_open = true,
            KeyCode::Esc if self.sidebar_overlay_open => self.sidebar_overlay_open = false,
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
                    match Route::parse(path, self.default_project.as_str()) {
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
            Some("new") => self.navigate(NavItem::NewTask),
            Some("help") => self.help_open = true,
            Some("sidebar") => self.toggle_sidebar(),
            Some("quit") => self.request_quit(),
            Some(unknown) => self.notice = Some(format!("unknown command: {unknown}")),
            None => {}
        }
    }

    fn apply_action(&mut self, action: ActionId) {
        match action {
            ActionId::Quit => self.request_quit(),
            ActionId::Tasks => self.navigate(NavItem::Tasks),
            ActionId::GlobalTasks => self.history.navigate(Route::GlobalTasks),
            ActionId::NewTask => self.navigate(NavItem::NewTask),
            ActionId::Inbox => self.navigate(NavItem::Inbox),
            ActionId::Ide => self.navigate(NavItem::Ide),
            ActionId::RepoGit => self.navigate(NavItem::RepoGit),
            ActionId::Github => self.navigate(NavItem::Github),
            ActionId::Skills => self.navigate(NavItem::Skills),
            ActionId::Workflows => self.navigate(NavItem::Workflows),
            ActionId::Settings => self.navigate(NavItem::Settings),
            ActionId::ToggleSidebar => self.toggle_sidebar(),
            ActionId::Help => self.help_open = true,
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
            HitAction::Tasks => self.navigate(NavItem::Tasks),
            HitAction::GlobalTasks => self.history.navigate(Route::GlobalTasks),
            HitAction::NewTask => self.navigate(NavItem::NewTask),
            HitAction::Inbox => self.navigate(NavItem::Inbox),
            HitAction::Ide => self.navigate(NavItem::Ide),
            HitAction::RepoGit => self.navigate(NavItem::RepoGit),
            HitAction::Github => self.navigate(NavItem::Github),
            HitAction::Skills => self.navigate(NavItem::Skills),
            HitAction::Workflows => self.navigate(NavItem::Workflows),
            HitAction::Settings => self.navigate(NavItem::Settings),
            HitAction::ActiveTasks => self.task_filter = TaskFilter::Active,
            HitAction::ArchivedTasks => self.task_filter = TaskFilter::Archived,
            HitAction::ToggleSidebar => self.toggle_sidebar(),
            HitAction::Help => self.help_open = true,
            HitAction::ProjectToggle(project) => {
                if let Some(entry) = self.projects.iter_mut().find(|entry| entry.id == project) {
                    entry.collapsed = !entry.collapsed;
                }
            }
            HitAction::SidebarEdge => self.sidebar_dragging = true,
            HitAction::Back => {
                self.history.back();
            }
            HitAction::Forward => {
                self.history.forward();
            }
            HitAction::Quit => self.request_quit(),
        }
    }

    fn navigate(&mut self, nav: NavItem) {
        let project = self.current_project().to_owned();
        if nav == NavItem::Tasks {
            self.history.navigate(Route::Tasks { project });
        } else {
            self.history.navigate(Route::Placeholder { project, nav });
        }
        self.notice = None;
    }

    fn route_is(&self, nav: NavItem) -> bool {
        matches!(self.route(), Route::Placeholder { nav: current, .. } if *current == nav)
            || (nav == NavItem::Tasks && matches!(self.route(), Route::Tasks { .. }))
    }

    fn toggle_sidebar(&mut self) {
        if self.last_width == 0 || self.last_width < SIDEBAR_BREAKPOINT {
            self.sidebar_overlay_open = !self.sidebar_overlay_open;
        } else {
            self.sidebar_collapsed = !self.sidebar_collapsed;
        }
    }

    fn request_quit(&mut self) {
        if self.running_count() > 0 {
            self.confirm_quit = true;
        } else {
            self.quit = true;
        }
    }

    fn provider_summary(&self) -> String {
        if self.providers.is_empty() {
            return "[providers --]".to_owned();
        }
        self.providers
            .iter()
            .map(|provider| {
                format!(
                    "[{} {}]",
                    provider.name,
                    if provider.available { "ok" } else { "--" }
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn normal_style(&self) -> Style {
        Style::default().fg(self.theme.palette.fg)
    }

    fn soft_style(&self) -> Style {
        Style::default().fg(self.theme.palette.soft_fg)
    }

    fn nav_style(&self, active: bool) -> Style {
        if active {
            self.active_style()
        } else {
            self.normal_style()
        }
    }

    fn active_style(&self) -> Style {
        Style::default()
            .fg(self.theme.palette.accent)
            .add_modifier(Modifier::BOLD)
    }
}

fn nav_hit_action(nav: NavItem) -> HitAction {
    match nav {
        NavItem::NewTask => HitAction::NewTask,
        NavItem::Tasks => HitAction::Tasks,
        NavItem::Inbox => HitAction::Inbox,
        NavItem::Ide => HitAction::Ide,
        NavItem::RepoGit => HitAction::RepoGit,
        NavItem::Github => HitAction::Github,
        NavItem::Skills => HitAction::Skills,
        NavItem::Workflows => HitAction::Workflows,
        NavItem::Settings => HitAction::Settings,
    }
}

fn sidebar_line(value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(Span::styled(value.into(), style))
}

fn sidebar_nav_line(
    label: &str,
    badge: Option<usize>,
    active: bool,
    style: Style,
) -> Line<'static> {
    let mut spans = vec![Span::styled(if active { "  > " } else { "    " }, style)];
    spans.push(Span::styled(label.to_owned(), style));
    if let Some(badge) = badge {
        spans.push(Span::styled(format!("  [{badge}]"), style));
    }
    Line::from(spans)
}

fn task_line(task: &QuickTask, style: Style) -> Line<'static> {
    let title = truncate(&task.title, 21);
    Line::from(vec![
        Span::styled("    + ", style),
        Span::styled(title, style),
    ])
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() && max > 1 {
        result.pop();
        result.push('~');
    }
    result
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn service_state_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Disabled => "off",
        ServiceState::Starting => "starting",
        ServiceState::Adopted => "attached",
        ServiceState::Ready => "ready",
        ServiceState::Failed => "failed",
        ServiceState::Stopped => "stopped",
    }
}

trait ThemeStatusPalette {
    fn palette_for_status(&self, status: RunStatus) -> Style;
}

impl ThemeStatusPalette for Theme {
    fn palette_for_status(&self, status: RunStatus) -> Style {
        let color = match status {
            RunStatus::Queued => self.palette.queued,
            RunStatus::Running => self.palette.running,
            RunStatus::Waiting => self.palette.waiting,
            RunStatus::Review => self.palette.review,
            RunStatus::Done => self.palette.done,
            RunStatus::Failed => self.palette.failed,
            RunStatus::Cancelled => self.palette.cancelled,
        };
        Style::default().fg(color)
    }
}

trait UppercaseTitle {
    fn uppercase_title(self) -> &'static str;
}

impl UppercaseTitle for NavItem {
    fn uppercase_title(self) -> &'static str {
        match self {
            Self::NewTask => "NEW TASK",
            Self::Tasks => "TASKS",
            Self::Inbox => "INBOX",
            Self::Ide => "IDE",
            Self::RepoGit => "GIT",
            Self::Github => "GITHUB",
            Self::Skills => "SKILLS",
            Self::Workflows => "WORKFLOWS",
            Self::Settings => "SETTINGS",
        }
    }
}

trait TaskGroupLabel {
    fn label(self) -> &'static str;
}

impl TaskGroupLabel for TaskGroup {
    fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "  NEEDS YOU",
            Self::Working => "  WORKING",
            Self::Recent => "  RECENT",
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyEvent, KeyModifiers, MouseEvent};
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
        assert_eq!(initial.path(), "/p/main");
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

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 4,
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
    fn workspace_events_update_live_shell_badges() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.apply_workspace_event(WorkspaceEvent::Run(QuickTask {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
            title: "Ship shell".to_owned(),
            status: RunStatus::Running,
            archived: false,
            unread: false,
            created_at: "2026-08-15T00:00:00Z".to_owned(),
        }));
        app.apply_workspace_event(WorkspaceEvent::Todos {
            project: "main".to_owned(),
            count: 3,
        });
        assert_eq!(app.running_count(), 1);
        assert_eq!(app.inbox_count(), 3);

        app.apply_workspace_event(WorkspaceEvent::Run(QuickTask {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
            title: "Ship shell".to_owned(),
            status: RunStatus::Review,
            archived: false,
            unread: true,
            created_at: "2026-08-15T00:00:00Z".to_owned(),
        }));
        assert_eq!(app.running_count(), 0);
        assert_eq!(app.needs_you_count(), 1);

        app.apply_workspace_event(WorkspaceEvent::RunDeleted {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });
        assert_eq!(app.needs_you_count(), 0);
    }

    #[test]
    fn sidebar_collapses_at_the_narrow_breakpoint_and_can_open_as_a_drawer() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        assert_eq!(app.sidebar_width(), SIDEBAR_DEFAULT_WIDTH);
        assert!(!app.sidebar_is_visible(80));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.sidebar_is_visible(80));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.sidebar_is_visible(80));
    }

    #[test]
    fn sidebar_edge_drag_updates_width() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: SIDEBAR_DEFAULT_WIDTH - 1,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 36,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 36,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.sidebar_width(), 37);
    }

    #[test]
    fn renders_at_the_three_a4_snapshot_sizes() {
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
