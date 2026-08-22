use std::collections::BTreeMap;
use std::path::PathBuf;

use coducktor_contract::{
    ApiRun, ProcessUsage, ProjectListEntry, RunIndexEntry, RunStatus, RunsIndexResponse,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::input::hitmap::{HitAction, HitMap};
use crate::input::keymap::{ActionId, KeyMode, Keymap};
use crate::input::neovim::{Direction as VimDirection, FeedResult, NeovimInput, NormalCommand};
use crate::screens::runs_util::TaskView;
use crate::theme::{Theme, ThemeName};
use crate::widgets::table::ColumnId;

const SIDEBAR_BREAKPOINT: u16 = 100;
const SIDEBAR_DEFAULT_WIDTH: u16 = 28;
const SIDEBAR_MIN_WIDTH: u16 = 20;
const SIDEBAR_MAX_WIDTH: u16 = 44;

/// Shell navigation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    NewTask,
    Tasks,
    Scratchpad,
    Ide,
    Terminal,
    RepoGit,
    Github,
    Skills,
    Workflows,
    Settings,
}

impl NavItem {
    const ALL: [Self; 9] = [
        Self::Tasks,
        Self::Scratchpad,
        Self::Ide,
        Self::Terminal,
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
            Self::Scratchpad => "Scratchpad",
            Self::Ide => "IDE",
            Self::Terminal => "Terminal",
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
            Self::Scratchpad => "scratchpad",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
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
            "scratchpad" | "notes" => Some(Self::Scratchpad),
            "ide" => Some(Self::Ide),
            "terminal" => Some(Self::Terminal),
            "git" | "repo-git" => Some(Self::RepoGit),
            "github" => Some(Self::Github),
            "skills" => Some(Self::Skills),
            "workflows" => Some(Self::Workflows),
            "settings" => Some(Self::Settings),
            _ => None,
        }
    }
}

/// One keyboard/mouse-navigable sidebar row. Project rows precede the current
/// project's nav rows, then the workspace entries and the task filter rows, so
/// the shared arrow selector can reach every destination the sidebar renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarRow {
    Project(usize),
    Nav(NavItem),
    GlobalTasks,
    GlobalSettings,
    Filter(TaskFilter),
}

/// The routed identity used by the TUI. Routes retain a stable URL-shaped seam for navigation.
/// A `screens/task_git` sub-tab — Changes / Files / Commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGitTab {
    Changes,
    Files,
    Commits,
}

impl TaskGitTab {
    fn path_segment(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Files => "files",
            Self::Commits => "commits",
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        match segment {
            "changes" => Some(Self::Changes),
            "files" => Some(Self::Files),
            "commits" => Some(Self::Commits),
            _ => None,
        }
    }
}

/// A `screens/repo_git` sub-tab — Commits / Changes / Branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoGitTab {
    Changes,
    Commits,
    Branches,
}

impl RepoGitTab {
    fn path_segment(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Commits => "commits",
            Self::Branches => "branches",
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        match segment {
            "changes" => Some(Self::Changes),
            "commits" => Some(Self::Commits),
            "branches" => Some(Self::Branches),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Tasks {
        project: String,
    },
    GlobalTasks,
    GlobalSettings,
    NewTask {
        project: String,
    },
    Scratchpad {
        project: String,
    },
    Thread {
        project: String,
        id: String,
    },
    TaskGit {
        project: String,
        id: String,
        tab: TaskGitTab,
    },
    Ide {
        project: String,
    },
    Terminal {
        project: String,
    },
    Github {
        project: String,
    },
    Skills {
        project: String,
    },
    Workflows {
        project: String,
    },
    RepoGit {
        project: String,
        tab: RepoGitTab,
    },
    Compare {
        project: String,
        group_id: String,
    },
    Settings {
        project: String,
    },
    Placeholder {
        project: String,
        nav: NavItem,
    },
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
        if path == "/settings" {
            return Some(Self::GlobalSettings);
        }
        if path == "/new" {
            return Some(Self::NewTask {
                project: default_project.to_owned(),
            });
        }
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts.first() == Some(&"p") {
            let project = (*parts.get(1)?).to_owned();
            return match parts.get(2).copied() {
                None => Some(Self::Tasks { project }),
                Some("new") => Some(Self::NewTask { project }),
                Some("scratchpad" | "notes") => Some(Self::Scratchpad { project }),
                Some("tasks") if parts.len() >= 5 => {
                    let id = (*parts.get(3)?).to_owned();
                    match TaskGitTab::parse(parts.get(4)?) {
                        Some(tab) => Some(Self::TaskGit { project, id, tab }),
                        None => Some(Self::Thread { project, id }),
                    }
                }
                Some("tasks") if parts.len() >= 4 => Some(Self::Thread {
                    project,
                    id: (*parts.get(3)?).to_owned(),
                }),
                Some("tasks") => Some(Self::Tasks { project }),
                Some("compare") if parts.len() >= 4 => Some(Self::Compare {
                    project,
                    group_id: (*parts.get(3)?).to_owned(),
                }),
                Some("ide") => Some(Self::Ide { project }),
                Some("terminal") => Some(Self::Terminal { project }),
                Some("github") => Some(Self::Github { project }),
                Some("skills") => Some(Self::Skills { project }),
                Some("workflows") => Some(Self::Workflows { project }),
                Some("git" | "repo-git") => {
                    let tab = parts
                        .get(3)
                        .and_then(|segment| RepoGitTab::parse(segment))
                        .unwrap_or(RepoGitTab::Commits);
                    Some(Self::RepoGit { project, tab })
                }
                Some("settings") => Some(Self::Settings { project }),
                Some(segment) => {
                    NavItem::parse(segment).map(|nav| Self::Placeholder { project, nav })
                }
            };
        }
        if parts.first() == Some(&"tasks") {
            return parts.get(1).map(|id| Self::Thread {
                project: default_project.to_owned(),
                id: (*id).to_owned(),
            });
        }
        None
    }

    pub fn path(&self) -> String {
        match self {
            Self::Tasks { project } => format!("/p/{project}"),
            Self::GlobalTasks => "/tasks".to_owned(),
            Self::GlobalSettings => "/settings".to_owned(),
            Self::NewTask { project } => format!("/p/{project}/new"),
            Self::Scratchpad { project } => format!("/p/{project}/scratchpad"),
            Self::Thread { project, id } => format!("/p/{project}/tasks/{id}"),
            Self::TaskGit { project, id, tab } => {
                format!("/p/{project}/tasks/{id}/{}", tab.path_segment())
            }
            Self::Ide { project } => format!("/p/{project}/ide"),
            Self::Terminal { project } => format!("/p/{project}/terminal"),
            Self::Github { project } => format!("/p/{project}/github"),
            Self::Skills { project } => format!("/p/{project}/skills"),
            Self::Workflows { project } => format!("/p/{project}/workflows"),
            Self::RepoGit { project, tab } => {
                format!("/p/{project}/repo-git/{}", tab.path_segment())
            }
            Self::Compare { project, group_id } => format!("/p/{project}/compare/{group_id}"),
            Self::Settings { project } => format!("/p/{project}/settings"),
            Self::Placeholder { project, nav } => {
                format!("/p/{project}/{}", nav.path_segment())
            }
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Tasks { .. } => "TASKS",
            Self::GlobalTasks => "GLOBAL TASKS",
            Self::GlobalSettings => "GLOBAL SETTINGS",
            Self::NewTask { .. } => "NEW TASK",
            Self::Scratchpad { .. } => "SCRATCHPAD",
            Self::Thread { .. } => "TASK THREAD",
            Self::TaskGit { .. } => "TASK GIT",
            Self::Ide { .. } => "IDE",
            Self::Terminal { .. } => "TERMINAL",
            Self::Github { .. } => "GITHUB",
            Self::Skills { .. } => "SKILLS",
            Self::Workflows { .. } => "WORKFLOWS",
            Self::RepoGit { .. } => "REPO GIT",
            Self::Compare { .. } => "COMPARE",
            Self::Settings { .. } => "SETTINGS",
            Self::Placeholder { nav, .. } => nav.uppercase_title(),
        }
    }

    fn project(&self) -> Option<&str> {
        match self {
            Self::Tasks { project }
            | Self::NewTask { project }
            | Self::Scratchpad { project }
            | Self::Thread { project, .. }
            | Self::TaskGit { project, .. }
            | Self::Ide { project }
            | Self::Terminal { project }
            | Self::Github { project }
            | Self::Skills { project }
            | Self::Workflows { project }
            | Self::RepoGit { project, .. }
            | Self::Compare { project, .. }
            | Self::Settings { project }
            | Self::Placeholder { project, .. } => Some(project),
            Self::GlobalTasks | Self::GlobalSettings => None,
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
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusLocation {
    Sidebar,
    Screen(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandId {
    Open,
    Back,
    Forward,
    Theme,
    New,
    ClearScratchpad,
    Help,
    Sidebar,
    Stop,
    Finish,
    Archive,
    Delete,
    Quit,
}

impl CommandId {
    const ALL: [Self; 13] = [
        Self::Open,
        Self::Back,
        Self::Forward,
        Self::Theme,
        Self::New,
        Self::ClearScratchpad,
        Self::Help,
        Self::Sidebar,
        Self::Stop,
        Self::Finish,
        Self::Archive,
        Self::Delete,
        Self::Quit,
    ];

    fn parse(name: &str) -> Option<Self> {
        match name {
            "open" => Some(Self::Open),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            "theme" => Some(Self::Theme),
            "new" => Some(Self::New),
            "clear-scratchpad" => Some(Self::ClearScratchpad),
            "help" => Some(Self::Help),
            "sidebar" => Some(Self::Sidebar),
            "stop" => Some(Self::Stop),
            "finish" => Some(Self::Finish),
            "archive" => Some(Self::Archive),
            "delete" => Some(Self::Delete),
            "q" | "quit" => Some(Self::Quit),
            _ => None,
        }
    }

    fn usage(self) -> &'static str {
        match self {
            Self::Open => ":open <route>",
            Self::Back => ":back",
            Self::Forward => ":forward",
            Self::Theme => ":theme <dark|lazyvim|lakes>",
            Self::New => ":new",
            Self::ClearScratchpad => ":clear-scratchpad",
            Self::Help => ":help",
            Self::Sidebar => ":sidebar",
            Self::Stop => ":stop",
            Self::Finish => ":finish",
            Self::Archive => ":archive",
            Self::Delete => ":delete",
            Self::Quit => ":q",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Open => "navigate to a route",
            Self::Back => "go back",
            Self::Forward => "go forward",
            Self::Theme => "switch theme",
            Self::New => "new task",
            Self::ClearScratchpad => "clear the current scratchpad",
            Self::Help => "open this help",
            Self::Sidebar => "toggle sidebar",
            Self::Stop => "stop the current task",
            Self::Finish => "finish the current task",
            Self::Archive => "archive the current task",
            Self::Delete => "delete the current task",
            Self::Quit => "quit",
        }
    }
}

/// The Active/Archived filter shared by the shell and the Tasks screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskFilter {
    Active,
    Archived,
}

/// Workspace-qualified task identity. A run id is only unique inside its project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskKey {
    pub project_id: String,
    pub run_id: String,
}

impl TaskKey {
    pub fn new(project_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            run_id: run_id.into(),
        }
    }
}

/// Cached task data for one registered project. The generation makes an A → B → A response
/// distinguishable even when the final active project is A again.
#[derive(Debug, Clone)]
pub struct ProjectTasksState {
    pub runs: Vec<ApiRun>,
    pub loading: bool,
    pub error: Option<String>,
    pub request_generation: u64,
    pub selection: Option<TaskKey>,
    pub scroll_y: usize,
    pub filter: TaskFilter,
    pub live_usage: BTreeMap<String, ProcessUsage>,
}

impl Default for ProjectTasksState {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            loading: false,
            error: None,
            request_generation: 0,
            selection: None,
            scroll_y: 0,
            filter: TaskFilter::Active,
            live_usage: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskGroup {
    NeedsYou,
    Working,
    Done,
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
            RunStatus::Queued | RunStatus::Running | RunStatus::Idle => TaskGroup::Working,
            RunStatus::Waiting | RunStatus::Review => TaskGroup::NeedsYou,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => TaskGroup::Done,
        }
    }
}

/// A run's status transition worth surfacing as a desktop notification: "needs you" (waiting on
/// an answer or a review) or "finished" (done/failed/cancelled) — but only the first time a run
/// enters one of those states, never on every repeated status event.
fn notification_for_transition(old: RunStatus, task: &QuickTask) -> Option<(String, String)> {
    if old == task.status {
        return None;
    }
    match task.status {
        RunStatus::Waiting => Some(("Needs your answer".to_owned(), task.title.clone())),
        RunStatus::Review => Some(("Ready for review".to_owned(), task.title.clone())),
        RunStatus::Done => Some(("Task finished".to_owned(), task.title.clone())),
        RunStatus::Failed => Some(("Task failed".to_owned(), task.title.clone())),
        _ => None,
    }
}

/// A single frame of workspace news from the `/workspace/events` stream. The
/// `Run` arm is intentionally wide — it carries a whole `ApiRun` so the table
/// can update a row in place without a refetch.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum WorkspaceEvent {
    Run {
        project: String,
        run: ApiRun,
    },
    RunDeleted {
        project: String,
        id: String,
    },
    Usage {
        project: String,
        usage: BTreeMap<String, ProcessUsage>,
    },
    ProviderStatus {
        provider: String,
        available: bool,
    },
    Lagged {
        count: usize,
    },
}

/// Sanitized local counters for runtime backpressure behavior. These values never include task
/// text, provider payloads, credentials, or file contents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppRuntimeMetrics {
    pub coalesced_workspace_run_updates: usize,
    pub frame_micros: u64,
    pub projection_micros: u64,
    pub events_reduced: usize,
    pub dropped_events: usize,
}

/// A mutation the shell or a screen wants the engine loop to run next frame.
/// Main owns the engine; the app only queues these.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    Archive {
        project: String,
        id: String,
        archived: bool,
    },
    Delete {
        project: String,
        id: String,
    },
    Read {
        project: String,
        id: String,
    },
    Unread {
        project: String,
        id: String,
    },
    RefreshTasks {
        project: String,
    },
    RefreshIndex,
    /// Refresh the registered-project list after a registry mutation completes.
    RefreshProjectRegistry,
    /// Start a new task with an already-assembled create-run body.
    StartRun {
        project: String,
        input: coducktor_contract::CreateRunInput,
    },
    /// Release accepted queued work only after the thread route has installed its live listener.
    ActivateRuns {
        project: String,
    },
    /// Load the new-task screen's per-project data (skills, workflows, config, …).
    RefreshNewTask {
        project: String,
    },
    LoadScratchpad {
        project: String,
    },
    /// Clear the current scratchpad after the user confirms the destructive action.
    ClearScratchpad {
        project: String,
    },
    SaveScratchpad {
        project: String,
        content: String,
    },
    /// Load the model catalog for one runner.
    RefreshModels {
        runner: coducktor_contract::Runner,
    },
    /// Persist the (bumped) ui-state map.
    PutUiState {
        project: String,
        state: coducktor_contract::UiState,
    },
    /// Change the project's configured base branch.
    SetBaseBranch {
        project: String,
        base_branch: Option<String>,
    },
    /// Load a run's detail, first history page, and live event stream.
    LoadThread {
        project: String,
        id: String,
    },
    LoadEarlierThread {
        project: String,
        id: String,
        cursor: String,
    },
    /// Deliver a message into the run's open session (or fold it into a queued prompt).
    SendMessage {
        project: String,
        id: String,
        input: coducktor_contract::MessageInput,
    },
    CancelRun {
        project: String,
        id: String,
    },
    /// Reopen a finished run's session — Continue, the review panel's Send back (prefixed by
    /// the caller), and an ask-answer resume all ride this one action.
    ContinueRun {
        project: String,
        id: String,
        text: Option<String>,
        images: Option<Vec<coducktor_contract::ImageInput>>,
    },
    FinishRun {
        project: String,
        id: String,
    },
    CreatePr {
        project: String,
        id: String,
    },
    OpenInCli {
        project: String,
        id: String,
    },
    RemoveQueuedMessage {
        project: String,
        id: String,
        message_id: String,
    },
    CancelAutoResume {
        project: String,
        id: String,
    },
    /// Load the task-git screen's Changes tab.
    LoadTaskGitChanges {
        project: String,
        id: String,
    },
    /// Load the task-git screen's Files tab at the given worktree path (`None` = root).
    LoadTaskGitFiles {
        project: String,
        id: String,
        path: Option<String>,
    },
    /// Load the task-git screen's Commits tab.
    LoadTaskGitCommits {
        project: String,
        id: String,
    },
    /// Load one of the run's commits, structured — the Commits tab's detail pane.
    LoadTaskGitCommitDiff {
        project: String,
        id: String,
        sha: String,
    },
    /// Commit the Changes tab's selected worktree changes.
    TaskGitCommit {
        project: String,
        id: String,
    },
    /// Push the Changes tab's task branch.
    TaskGitPush {
        project: String,
        id: String,
    },
    /// Load the repo-git screen.
    LoadRepoGit {
        project: String,
    },
    LoadRepoGitCommits {
        project: String,
    },
    LoadRepoGitCommitDiff {
        project: String,
        sha: String,
    },
    /// Create or check out a branch from the Branches tab.
    RepoGitBranch {
        project: String,
        name: String,
        from: Option<String>,
    },
    /// Load the compare-variants screen.
    LoadCompare {
        project: String,
        group_id: String,
    },
    /// Pick a winning run from the compare view.
    PickVariant {
        project: String,
        group_id: String,
        run_id: String,
    },
    /// Load one compare variant's full structured diff on demand.
    LoadCompareVariantDiff {
        project: String,
        group_id: String,
        run_id: String,
    },
    /// Load the IDE's explorer at a project-relative directory (`None` = root).
    LoadIdeDirectory {
        project: String,
        path: Option<String>,
    },
    /// Load the IDE's file — replace the draft and clear dirty state.
    LoadIdeFile {
        project: String,
        path: String,
    },
    /// Save the IDE's file from the editor's `Ctrl+S` action.
    SaveIdeFile {
        project: String,
        path: String,
    },
    /// The IDE's `Ctrl+E` escape hatch: resolve the absolute path and hand the file to
    /// `$EDITOR` (main.rs suspends the terminal, spawns, resumes).
    OpenIdeInEditor {
        project: String,
        path: String,
    },
    /// Unsaved-changes guard resolutions: the user confirmed discarding the
    /// IDE draft, now perform the deferred navigation.
    IdeDiscardThenNavigate(Box<Route>),
    IdeDiscardThenBack,
    IdeDiscardThenForward,
    /// Switch the active project after the IDE's unsaved-changes guard is confirmed.
    SwitchProject(String),
    /// Load the GitHub screen's aggregate data.
    LoadGithub {
        project: String,
    },
    /// Load the hand-to-agent workflow and skill pickers.
    LoadGithubPickers {
        project: String,
    },
    LoadGithubComments {
        project: String,
        kind: String,
        number: u64,
    },
    LoadGithubMergeState {
        project: String,
        number: u64,
    },
    LoadGithubPrChanges {
        project: String,
        number: u64,
    },
    /// Merge a GitHub pull request from the merge-gate confirmation.
    GithubMerge {
        project: String,
        number: u64,
        method: coducktor_contract::GithubMergeMethod,
        head_sha: String,
        override_rules: bool,
    },
    /// The hand-to-agent's run start — like `StartRun` but stays on the GitHub screen and
    /// reports the queued run id in the hand-off card instead of navigating away.
    GithubHandToAgent {
        project: String,
        input: coducktor_contract::CreateRunInput,
    },
    LoadSkills {
        project: String,
    },
    LoadWorkflows {
        project: String,
    },
    LoadWorkflowSkills {
        project: String,
    },
    /// Save the draft in `skills:` form when portable and `steps:` otherwise.
    SaveWorkflow {
        project: String,
    },
    /// The export path: the same write, answered with the file path it landed in.
    ExportWorkflow {
        project: String,
    },
    DeleteWorkflow {
        project: String,
        name: String,
    },
    ImportWorkflow {
        project: String,
        yaml: String,
    },
    /// Load every Settings data source for the current project and workspace.
    LoadSettings {
        project: String,
    },
    SettingsPutConfig {
        project: String,
        input: coducktor_contract::SetConfigInput,
    },
    SettingsPutWorkspaceConfig {
        input: coducktor_contract::SetWorkspaceConfigInput,
    },
    SettingsPutWorkspaceUiState {
        input: coducktor_contract::WorkspaceUiState,
    },
    /// Open one agent-config file's raw content into the settings editor.
    SettingsLoadConfigFile {
        project: String,
        id: String,
    },
    SettingsPutConfigFile {
        project: String,
        id: String,
        content: String,
        version: Option<String>,
    },
    SettingsCreateAgentProfile {
        provider: coducktor_contract::Runner,
        config_dir: String,
    },
    SettingsUpdateAgentProfile {
        id: String,
        input: coducktor_contract::UpdateAgentProfileInput,
    },
    SettingsRemoveAgentProfile {
        id: String,
    },
    SettingsSelectAgentProfile {
        input: coducktor_contract::SelectAgentProfileInput,
    },
    ConnectProvider {
        input: coducktor_contract::ProviderConnectInput,
    },
    SettingsRegisterProject {
        root: String,
    },
    SettingsReclaimWorktrees {
        project: String,
    },
    SettingsRemoveWorktree {
        project: String,
        run_id: String,
    },
    SettingsRemoveProject {
        id: String,
    },
    SettingsUpdateProject {
        id: String,
        input: coducktor_contract::UpdateProjectInput,
    },
    Quit,
}

impl PendingAction {
    pub(crate) fn is_coalescable_refresh(&self) -> bool {
        matches!(
            self,
            Self::RefreshTasks { .. }
                | Self::RefreshIndex
                | Self::RefreshProjectRegistry
                | Self::RefreshNewTask { .. }
                | Self::RefreshModels { .. }
        )
    }
}

/// A blocking question rendered over the shell; confirmed with `y`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmRequest {
    pub text: String,
    pub action: PendingAction,
}

/// The row menu overlay opened from a table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMenu {
    pub project: String,
    pub run_id: String,
    pub title: String,
    pub items: Vec<RowMenuItem>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMenuItem {
    pub label: String,
    pub action: MenuAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    Archive,
    Restore,
    MarkRead,
    MarkUnread,
    Delete,
    OpenPr,
    CopyBranch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderBadge {
    name: String,
    available: bool,
}

/// The terminal shell state shared by content screens.
pub struct App {
    pub history: History,
    pub hitmap: HitMap,
    pub theme: Theme,
    keymap: Keymap,
    mode: InputMode,
    command: String,
    search: String,
    last_search: String,
    normal_input: NeovimInput,
    pub notice: Option<String>,
    toast: Option<String>,
    pub hover: Option<(u16, u16)>,
    quit: bool,
    help_open: bool,
    pub confirm: Option<ConfirmRequest>,
    pub row_menu: Option<RowMenu>,
    pub default_project: String,
    pub projects: Vec<ProjectEntry>,
    quick_tasks: Vec<QuickTask>,
    pub task_filter: TaskFilter,
    sidebar_width: u16,
    sidebar_collapsed: bool,
    sidebar_overlay_open: bool,
    sidebar_dragging: bool,
    pub last_width: u16,
    providers: Vec<ProviderBadge>,
    pub tasks: Vec<ApiRun>,
    /// Project-keyed task cache. `tasks` remains a compatibility view of the active project's
    /// entry for existing screens and embedders while migration proceeds.
    pub project_tasks: BTreeMap<String, ProjectTasksState>,
    pub global_index: Option<RunsIndexResponse>,
    pub global_filter: TaskFilter,
    pub global_loading: bool,
    pub global_error: Option<String>,
    pub global_request_generation: u64,
    pub project_registry: Vec<ProjectListEntry>,
    /// The launch directory (`--repo` or the working directory), used by the
    /// embedded terminal tab as the boot project's root before the registry loads.
    pub boot_root: Option<PathBuf>,
    pub live_usage: BTreeMap<String, ProcessUsage>,
    pub now_epoch: i64,
    pub animation_tick: u64,
    pub tasks_ui: crate::screens::tasks::TasksUi,
    pub global_ui: crate::screens::global_tasks::GlobalUi,
    pub new_task_ui: crate::screens::new_task::NewTaskUi,
    /// Per-project generations reject a stale New Task snapshot after an A → B → A switch.
    new_task_request_generations: BTreeMap<String, u64>,
    pub scratchpad_ui: crate::screens::scratchpad::ScratchpadUi,
    /// Rejects a stale hydration after navigating away from and back to a scratchpad.
    pub scratchpad_request_generation: u64,
    /// Per-project New Task drafts, keyed by project id. Survives navigation and
    /// project switching for the lifetime of the cockpit (a TUI has no reload).
    pub new_task_drafts: BTreeMap<String, crate::new_task_form::NewTaskDraft>,
    /// Per-project full composer state, including clipboard payloads that are not in draft text.
    pub new_task_composers: BTreeMap<String, crate::widgets::composer::Composer>,
    /// Drafts captured at submit time, restored if the scoped start fails after navigation.
    pub pending_start_drafts: BTreeMap<String, crate::new_task_form::NewTaskDraft>,
    /// Full composer snapshots retain clipboard image bytes and compact large-paste payloads.
    pub pending_start_composers: BTreeMap<String, crate::widgets::composer::Composer>,
    pub thread_ui: crate::screens::thread::ThreadUi,
    /// Rejects an older full thread reload after a newer request for the active run.
    pub thread_request_generation: u64,
    pub task_git_ui: crate::screens::task_git::TaskGitUi,
    /// Rejects an older Task Git aggregate load after returning to the same task.
    pub task_git_request_generation: u64,
    pub repo_git_ui: crate::screens::repo_git::RepoGitUi,
    /// Rejects an older repository Git refresh after revisiting the same project.
    pub repo_git_request_generation: u64,
    pub compare_ui: crate::screens::compare::CompareUi,
    /// Rejects a stale aggregate compare load after returning to a group.
    pub compare_request_generation: u64,
    pub ide_ui: crate::screens::ide::IdeUi,
    pub github_ui: crate::screens::github::GithubUi,
    /// Rejects an older GitHub aggregate refresh after the screen is reopened.
    pub github_request_generation: u64,
    pub terminal_ui: crate::screens::terminal::TerminalUi,
    pub skills_ui: crate::screens::skills::SkillsUi,
    pub workflows_ui: crate::screens::workflows::WorkflowsUi,
    pub settings_ui: crate::screens::settings::SettingsUi,
    pub settings_request_generation: u64,
    pub palette: crate::overlay::Palette,
    /// Settings → Notifications' toggle, loaded once at startup and kept live by every write
    /// Gates `pending_notifications`, never the terminal-title update.
    pub notifications_enabled: bool,
    /// (summary, body) pairs main.rs drains once per tick and fires via `notify-rust`.
    pub pending_notifications: Vec<(String, String)>,
    pub pending: Vec<PendingAction>,
    /// Coalescable refresh actions the runtime has already dispatched to a background worker and
    /// not yet resolved. `queue_pending`'s dedup only sees `pending` itself, so it cannot stop a
    /// later frame's submission once the earlier one has been drained and dispatched — many
    /// production call sites also push onto `pending` directly rather than through
    /// `queue_pending`. This is the second, dispatch-time layer that closes both gaps: checked
    /// immediately before `execute_pending` would otherwise spawn a duplicate, regardless of how
    /// the action reached the queue.
    in_flight_coalescable: Vec<PendingAction>,
    runtime_metrics: AppRuntimeMetrics,
    debug_hud: bool,
    /// The absolute path main.rs should hand to `$EDITOR` (set by the `OpenIdeInEditor`
    /// handler; consumed by the run loop, which owns the terminal).
    pub editor_handoff: Option<String>,
    pub sort_picker_index: usize,
    /// Whether keyboard navigation is currently in the shell's left navigation panel.
    sidebar_focus: bool,
    sidebar_selected: usize,
    /// Focused pane inside the current screen for routes that do not have a screen-specific
    /// focus enum. Pane zero is the screen's leftmost pane.
    screen_focus: usize,
    previous_focus: Option<FocusLocation>,
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
            search: String::new(),
            last_search: String::new(),
            normal_input: NeovimInput::default(),
            notice: None,
            toast: None,
            hover: None,
            quit: false,
            help_open: false,
            confirm: None,
            row_menu: None,
            default_project: project.clone(),
            projects: vec![ProjectEntry {
                id: project.clone(),
                name: project,
                collapsed: false,
            }],
            quick_tasks: Vec::new(),
            task_filter: TaskFilter::Active,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            sidebar_collapsed: false,
            sidebar_overlay_open: false,
            sidebar_dragging: false,
            last_width: 0,
            providers: Vec::new(),
            tasks: Vec::new(),
            project_tasks: BTreeMap::new(),
            global_index: None,
            global_filter: TaskFilter::Active,
            global_loading: false,
            global_error: None,
            global_request_generation: 0,
            project_registry: Vec::new(),
            boot_root: None,
            live_usage: BTreeMap::new(),
            now_epoch: 0,
            animation_tick: 0,
            tasks_ui: crate::screens::tasks::TasksUi::default(),
            global_ui: crate::screens::global_tasks::GlobalUi::default(),
            new_task_ui: crate::screens::new_task::NewTaskUi::default(),
            new_task_request_generations: BTreeMap::new(),
            scratchpad_ui: crate::screens::scratchpad::ScratchpadUi::default(),
            scratchpad_request_generation: 0,
            new_task_drafts: BTreeMap::new(),
            new_task_composers: BTreeMap::new(),
            pending_start_drafts: BTreeMap::new(),
            pending_start_composers: BTreeMap::new(),
            thread_ui: crate::screens::thread::ThreadUi::default(),
            thread_request_generation: 0,
            task_git_ui: crate::screens::task_git::TaskGitUi::default(),
            task_git_request_generation: 0,
            repo_git_ui: crate::screens::repo_git::RepoGitUi::default(),
            repo_git_request_generation: 0,
            compare_ui: crate::screens::compare::CompareUi::default(),
            compare_request_generation: 0,
            ide_ui: crate::screens::ide::IdeUi::default(),
            github_ui: crate::screens::github::GithubUi::default(),
            github_request_generation: 0,
            terminal_ui: crate::screens::terminal::TerminalUi::default(),
            skills_ui: crate::screens::skills::SkillsUi::default(),
            workflows_ui: crate::screens::workflows::WorkflowsUi::default(),
            settings_ui: crate::screens::settings::SettingsUi::default(),
            settings_request_generation: 0,
            palette: crate::overlay::Palette::default(),
            notifications_enabled: false,
            pending_notifications: Vec::new(),
            pending: Vec::new(),
            in_flight_coalescable: Vec::new(),
            runtime_metrics: AppRuntimeMetrics::default(),
            debug_hud: false,
            editor_handoff: None,
            sort_picker_index: 0,
            // Start on the sidebar so `Ctrl-W l` enters the initial Tasks screen.
            sidebar_focus: true,
            sidebar_selected: 0,
            screen_focus: 0,
            previous_focus: None,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Return sanitized runtime counters for diagnostics and scaling tests.
    pub fn runtime_metrics(&self) -> AppRuntimeMetrics {
        self.runtime_metrics
    }

    /// Enable the sanitized status-bar diagnostics requested by `DUCK_DEBUG_HUD=1`.
    pub fn set_debug_hud(&mut self, enabled: bool) {
        self.debug_hud = enabled;
    }

    /// Record the previous frame's total and thread-projection work for the next HUD paint.
    pub fn record_frame_metrics(
        &mut self,
        frame_micros: u64,
        projection_micros: u64,
        events_reduced: usize,
    ) {
        self.runtime_metrics.frame_micros = frame_micros;
        self.runtime_metrics.projection_micros = projection_micros;
        self.runtime_metrics.events_reduced = events_reduced;
    }

    /// Account for live events the engine reports as dropped. Phase 2 wires the event bus to
    /// this counter; defining it with the HUD keeps that later change additive.
    pub fn record_dropped_events(&mut self, count: usize) {
        self.runtime_metrics.dropped_events =
            self.runtime_metrics.dropped_events.saturating_add(count);
    }

    /// Account for superseded whole-record updates after a bounded receiver batch is folded.
    pub fn record_coalesced_workspace_run_updates(&mut self, count: usize) {
        self.runtime_metrics.coalesced_workspace_run_updates = self
            .runtime_metrics
            .coalesced_workspace_run_updates
            .saturating_add(count);
    }

    pub(crate) fn screen_focus(&self) -> usize {
        self.current_screen_pane()
    }

    pub(crate) fn set_screen_focus(&mut self, pane: usize) {
        self.screen_focus = pane;
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
        if let Some(index) = self
            .projects
            .iter()
            .position(|entry| entry.id == self.current_project())
        {
            self.sidebar_selected = index;
        }
    }

    /// The keyboard/mouse-navigable sidebar rows in visual order.
    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut rows = Vec::new();
        for (index, project) in self.projects.iter().enumerate() {
            rows.push(SidebarRow::Project(index));
            if project.id == self.current_project() && !project.collapsed {
                rows.push(SidebarRow::Nav(NavItem::Tasks));
                for nav in NavItem::ALL.into_iter().skip(1) {
                    rows.push(SidebarRow::Nav(nav));
                }
            }
        }
        rows.push(SidebarRow::GlobalTasks);
        rows.push(SidebarRow::GlobalSettings);
        rows
    }

    fn sidebar_selected_row(&self) -> Option<SidebarRow> {
        self.sidebar_rows().get(self.sidebar_selected).copied()
    }

    /// The row for a sidebar hit action, so mouse hover/click moves the same
    /// arrow selector the keyboard uses.
    fn sidebar_row_for_hit(&self, action: &HitAction) -> Option<SidebarRow> {
        match action {
            HitAction::ProjectToggle(project) => self
                .projects
                .iter()
                .position(|entry| entry.id == *project)
                .map(SidebarRow::Project),
            HitAction::NewTask => Some(SidebarRow::Nav(NavItem::NewTask)),
            HitAction::Scratchpad => Some(SidebarRow::Nav(NavItem::Scratchpad)),
            HitAction::Tasks => Some(SidebarRow::Nav(NavItem::Tasks)),
            HitAction::Ide => Some(SidebarRow::Nav(NavItem::Ide)),
            HitAction::Terminal => Some(SidebarRow::Nav(NavItem::Terminal)),
            HitAction::RepoGit => Some(SidebarRow::Nav(NavItem::RepoGit)),
            HitAction::Github => Some(SidebarRow::Nav(NavItem::Github)),
            HitAction::Skills => Some(SidebarRow::Nav(NavItem::Skills)),
            HitAction::Workflows => Some(SidebarRow::Nav(NavItem::Workflows)),
            HitAction::Settings => Some(SidebarRow::Nav(NavItem::Settings)),
            HitAction::GlobalTasks => Some(SidebarRow::GlobalTasks),
            HitAction::GlobalSettings => Some(SidebarRow::GlobalSettings),
            HitAction::ActiveTasks => Some(SidebarRow::Filter(TaskFilter::Active)),
            HitAction::ArchivedTasks => Some(SidebarRow::Filter(TaskFilter::Archived)),
            _ => None,
        }
    }

    fn sidebar_position(&self, row: SidebarRow) -> Option<usize> {
        self.sidebar_rows()
            .iter()
            .position(|candidate| *candidate == row)
    }

    pub fn set_quick_tasks(&mut self, tasks: impl IntoIterator<Item = QuickTask>) {
        self.quick_tasks = tasks.into_iter().collect();
    }

    pub fn set_provider_states(&mut self, states: impl IntoIterator<Item = (String, bool)>) {
        self.providers = states
            .into_iter()
            .map(|(name, available)| ProviderBadge { name, available })
            .collect();
    }

    /// Replace the current project's run list.
    pub fn set_tasks(&mut self, runs: Vec<ApiRun>) {
        let project = self.current_project().to_owned();
        self.set_tasks_for_project(project, runs);
    }

    /// Replace one project's cached list without allowing a background response for another
    /// project to overwrite the active compatibility view.
    pub fn set_tasks_for_project(&mut self, project: String, runs: Vec<ApiRun>) {
        let state = self.project_tasks.entry(project.clone()).or_default();
        state.runs = runs;
        state.loading = false;
        state.error = None;
        if self.current_project() == project {
            self.sync_active_project_tasks();
        }
        self.tasks_ui.table.select(self.tasks_ui.table.selected);
    }

    pub fn begin_task_request(&mut self, project: &str) -> u64 {
        let state = self.project_tasks.entry(project.to_owned()).or_default();
        state.request_generation = state.request_generation.wrapping_add(1);
        state.loading = true;
        state.error = None;
        state.request_generation
    }

    pub fn begin_new_task_request(&mut self, project: &str) -> u64 {
        let generation = self
            .new_task_request_generations
            .entry(project.to_owned())
            .or_default();
        *generation = generation.wrapping_add(1);
        *generation
    }

    pub fn begin_settings_request(&mut self) -> u64 {
        self.settings_request_generation = self.settings_request_generation.wrapping_add(1);
        self.settings_request_generation
    }

    pub fn begin_github_request(&mut self) -> u64 {
        self.github_request_generation = self.github_request_generation.wrapping_add(1);
        self.github_request_generation
    }

    pub fn begin_scratchpad_request(&mut self) -> u64 {
        self.scratchpad_request_generation = self.scratchpad_request_generation.wrapping_add(1);
        self.scratchpad_request_generation
    }

    pub fn begin_compare_request(&mut self) -> u64 {
        self.compare_request_generation = self.compare_request_generation.wrapping_add(1);
        self.compare_request_generation
    }

    pub fn begin_repo_git_request(&mut self) -> u64 {
        self.repo_git_request_generation = self.repo_git_request_generation.wrapping_add(1);
        self.repo_git_request_generation
    }

    pub fn begin_task_git_request(&mut self) -> u64 {
        self.task_git_request_generation = self.task_git_request_generation.wrapping_add(1);
        self.task_git_request_generation
    }

    pub fn begin_thread_request(&mut self) -> u64 {
        self.thread_request_generation = self.thread_request_generation.wrapping_add(1);
        self.thread_request_generation
    }

    pub fn accepts_new_task_response(&self, project: &str, generation: u64) -> bool {
        self.new_task_request_generations.get(project) == Some(&generation)
    }

    pub fn apply_task_response(
        &mut self,
        project: &str,
        generation: u64,
        result: Result<Vec<ApiRun>, String>,
    ) -> bool {
        if self
            .project_tasks
            .entry(project.to_owned())
            .or_default()
            .request_generation
            != generation
        {
            return false;
        }
        match result {
            Ok(runs) => self.set_tasks_for_project(project.to_owned(), runs),
            Err(error) => {
                let state = self.project_tasks.entry(project.to_owned()).or_default();
                state.loading = false;
                state.error = Some(error);
                if self.current_project() == project {
                    self.sync_active_project_tasks();
                }
            }
        }
        true
    }

    fn sync_active_project_tasks(&mut self) {
        let project = self.current_project().to_owned();
        let (runs, usage, filter, selection, scroll_y) = {
            let state = self.project_tasks.entry(project.clone()).or_default();
            (
                state.runs.clone(),
                state.live_usage.clone(),
                state.filter,
                state.selection.clone(),
                state.scroll_y,
            )
        };
        self.tasks = runs;
        self.live_usage = usage;
        self.task_filter = filter;
        self.tasks_ui.table.scroll_y = scroll_y;
        if let Some(selection) = selection {
            self.tasks_ui.table.selected = self
                .tasks
                .iter()
                .position(|run| run.record.id == selection.run_id);
        }
        self.tasks_ui.table.select(self.tasks_ui.table.selected);
    }

    pub fn select_task(&mut self, project: &str, run_id: &str) {
        let state = self.project_tasks.entry(project.to_owned()).or_default();
        state.selection = Some(TaskKey::new(project, run_id));
    }

    fn persist_active_task_ui(&mut self) {
        let project = self.current_project().to_owned();
        if let Some(state) = self.project_tasks.get_mut(&project) {
            state.scroll_y = self.tasks_ui.table.scroll_y;
            if let Some((_, row)) = self.tasks_ui.table.selected_row() {
                state.selection = Some(TaskKey::new(project, row.key.clone()));
            }
        }
    }

    pub fn task_state(&self, project: &str) -> Option<&ProjectTasksState> {
        self.project_tasks.get(project)
    }

    pub fn set_global_index(&mut self, index: RunsIndexResponse) {
        self.global_index = Some(index);
        self.global_loading = false;
        self.global_error = None;
    }

    pub fn begin_global_index_request(&mut self) -> u64 {
        self.global_request_generation = self.global_request_generation.wrapping_add(1);
        self.global_loading = true;
        self.global_error = None;
        self.global_request_generation
    }

    pub fn apply_global_index_response(
        &mut self,
        generation: u64,
        result: Result<RunsIndexResponse, String>,
    ) -> bool {
        if self.global_request_generation != generation {
            return false;
        }
        match result {
            Ok(index) => self.set_global_index(index),
            Err(error) => {
                self.global_loading = false;
                self.global_error = Some(error);
            }
        }
        true
    }

    pub fn set_project_registry(&mut self, projects: Vec<ProjectListEntry>) {
        self.project_registry = projects;
    }

    /// Remember the launch directory so the embedded terminal tab can start a shell
    /// for the boot project before the registry has loaded.
    pub fn set_boot_root(&mut self, root: PathBuf) {
        self.boot_root = Some(root);
    }

    pub fn take_pending(&mut self) -> Vec<PendingAction> {
        std::mem::take(&mut self.pending)
    }

    /// Queue an action, collapsing an exact duplicate of a safe refresh already awaiting a frame.
    /// Mutations remain ordered and are never coalesced.
    pub fn queue_pending(&mut self, action: PendingAction) {
        if action.is_coalescable_refresh() && self.pending.iter().any(|queued| queued == &action) {
            return;
        }
        self.pending.push(action);
    }

    /// Drain at most `limit` queued actions, retaining the FIFO tail for a later frame.
    pub fn take_pending_up_to(&mut self, limit: usize) -> Vec<PendingAction> {
        let count = limit.min(self.pending.len());
        self.pending.drain(..count).collect()
    }

    /// Whether an identical coalescable refresh is already dispatched to a background worker and
    /// still unresolved. `execute_pending` checks this immediately before it would otherwise spawn
    /// one, for every action reaching that point regardless of which of the many `pending.push`
    /// call sites (or `queue_pending`) put it there.
    pub fn coalescable_in_flight(&self, action: &PendingAction) -> bool {
        self.in_flight_coalescable.contains(action)
    }

    /// Record a coalescable action as dispatched. Callers must check
    /// [`Self::coalescable_in_flight`] first — this does not itself dedupe, so calling it twice
    /// for the same action would need two matching [`Self::finish_coalescable_dispatch`] calls to
    /// fully clear.
    pub fn begin_coalescable_dispatch(&mut self, action: PendingAction) {
        self.in_flight_coalescable.push(action);
    }

    /// Clear one dispatched coalescable action once its result has arrived — accepted or
    /// discarded as stale, it no longer needs to block a later identical submission. A caller
    /// reconstructs the exact `PendingAction` its `BackgroundResult` corresponds to.
    pub fn finish_coalescable_dispatch(&mut self, action: &PendingAction) {
        if let Some(index) = self
            .in_flight_coalescable
            .iter()
            .position(|queued| queued == action)
        {
            self.in_flight_coalescable.remove(index);
        }
    }

    pub fn take_pending_notifications(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_notifications)
    }

    /// Navigate, guarding the IDE's unsaved draft: leaving a dirty file asks
    /// first, and the confirm's action resolves into the deferred `history.navigate` in
    /// main.rs. Every path that can move the app away from `Route::Ide` routes through this
    /// or `request_back`/`request_forward`.
    pub fn request_navigate(&mut self, route: Route) {
        if self.ide_ui.dirty && !matches!(route, Route::Ide { .. }) {
            self.confirm = Some(ConfirmRequest {
                text: "Discard unsaved changes and leave the IDE?".to_owned(),
                action: PendingAction::IdeDiscardThenNavigate(Box::new(route)),
            });
        } else {
            self.navigate_route(route);
        }
    }

    /// Navigate and re-anchor the sidebar selector on the destination's row, so
    /// the arrow selector and the current-route highlight never sit on two rows.
    /// Every entry point into the IDE converges here (sidebar, palette, `:open`,
    /// deferred discard-confirms), so the project sync and the root listing
    /// queue live with the navigation, not with one caller.
    pub(crate) fn navigate_route(&mut self, route: Route) {
        self.persist_active_task_ui();
        if let Route::Ide { project } = &route
            && self.ide_ui.project != *project
        {
            self.ide_ui = crate::screens::ide::IdeUi {
                project: project.clone(),
                ..crate::screens::ide::IdeUi::default()
            };
        }
        self.history.navigate(route);
        self.sidebar_focus = false;
        self.screen_focus = 0;
        self.sync_active_project_tasks();
        if let Route::Ide { project } = self.route() {
            let path = if self.ide_ui.directory_path.is_empty() {
                None
            } else {
                Some(self.ide_ui.directory_path.clone())
            };
            self.pending.push(PendingAction::LoadIdeDirectory {
                project: project.clone(),
                path,
            });
        }
        self.anchor_sidebar_selection();
    }

    /// Back, guarding the IDE's unsaved draft like `request_navigate`.
    pub fn request_back(&mut self) {
        if self.ide_ui.dirty && !self.history.back.is_empty() {
            self.confirm = Some(ConfirmRequest {
                text: "Discard unsaved changes and leave the IDE?".to_owned(),
                action: PendingAction::IdeDiscardThenBack,
            });
        } else {
            self.go_back();
        }
    }

    fn go_back(&mut self) {
        self.persist_active_task_ui();
        if self.history.back() {
            self.sidebar_focus = false;
            self.screen_focus = 0;
            self.sync_active_project_tasks();
            self.anchor_sidebar_selection();
        }
    }

    /// Forward, guarding the IDE's unsaved draft like `request_navigate`.
    pub fn request_forward(&mut self) {
        if self.ide_ui.dirty && !self.history.forward.is_empty() {
            self.confirm = Some(ConfirmRequest {
                text: "Discard unsaved changes and leave the IDE?".to_owned(),
                action: PendingAction::IdeDiscardThenForward,
            });
        } else {
            self.go_forward();
        }
    }

    fn go_forward(&mut self) {
        self.persist_active_task_ui();
        if self.history.forward() {
            self.sidebar_focus = false;
            self.screen_focus = 0;
            self.sync_active_project_tasks();
            self.anchor_sidebar_selection();
        }
    }

    /// Snap the sidebar selector to the current route's sidebar row after a
    /// navigation. Routes without a sidebar row (task Git tabs, compare) leave
    /// the selector where it is.
    fn anchor_sidebar_selection(&mut self) {
        let row = match self.route() {
            Route::Placeholder { nav, .. } => SidebarRow::Nav(*nav),
            Route::Tasks { .. } | Route::Thread { .. } => SidebarRow::Nav(NavItem::Tasks),
            Route::NewTask { .. } => SidebarRow::Nav(NavItem::Tasks),
            Route::Scratchpad { .. } => SidebarRow::Nav(NavItem::Scratchpad),
            Route::Ide { .. } => SidebarRow::Nav(NavItem::Ide),
            Route::Terminal { .. } => SidebarRow::Nav(NavItem::Terminal),
            Route::Github { .. } => SidebarRow::Nav(NavItem::Github),
            Route::Skills { .. } => SidebarRow::Nav(NavItem::Skills),
            Route::Workflows { .. } => SidebarRow::Nav(NavItem::Workflows),
            Route::RepoGit { .. } => SidebarRow::Nav(NavItem::RepoGit),
            Route::Settings { .. } => SidebarRow::Nav(NavItem::Settings),
            Route::GlobalTasks => SidebarRow::GlobalTasks,
            Route::GlobalSettings => SidebarRow::GlobalSettings,
            Route::TaskGit { .. } | Route::Compare { .. } => return,
        };
        if let Some(index) = self.sidebar_position(row) {
            self.sidebar_selected = index;
        }
    }

    pub fn set_editor_handoff(&mut self, path: String) {
        self.editor_handoff = Some(path);
    }

    pub fn take_editor_handoff(&mut self) -> Option<String> {
        self.editor_handoff.take()
    }

    pub fn task_view(&self) -> TaskView {
        let filter = if matches!(self.route(), Route::GlobalTasks) {
            self.global_filter
        } else {
            self.project_tasks
                .get(self.current_project())
                .map(|state| state.filter)
                .unwrap_or(self.task_filter)
        };
        match filter {
            TaskFilter::Active => TaskView::Active,
            TaskFilter::Archived => TaskView::Archived,
        }
    }

    pub fn toggle_view(&mut self) {
        let next = match self.task_view() {
            TaskView::Active => TaskFilter::Archived,
            TaskView::Archived => TaskFilter::Active,
        };
        if matches!(self.route(), Route::GlobalTasks) {
            self.global_filter = next;
        } else {
            let project = self.current_project().to_owned();
            self.project_tasks.entry(project).or_default().filter = next;
            self.task_filter = next;
        }
    }

    pub fn set_task_filter(&mut self, filter: TaskFilter) {
        if matches!(self.route(), Route::GlobalTasks) {
            self.global_filter = filter;
        } else {
            let project = self.current_project().to_owned();
            self.project_tasks.entry(project).or_default().filter = filter;
            self.task_filter = filter;
        }
    }

    /// The honest "capped" note the global screen renders when the index was
    /// truncated (§8.2) — a capped list must say it is capped.
    pub fn truncated_note(&self) -> String {
        let Some(index) = &self.global_index else {
            return String::new();
        };
        if index.truncated.is_empty() {
            return String::new();
        }
        format!(
            "Showing the newest {} tasks per project — older ones in {} are only in that project's Tasks page.",
            index.per_project_limit,
            index.truncated.join(", ")
        )
    }

    /// Open a URL in the platform browser, best-effort.
    /// Open a URL in the OS default handler. This is best-effort and does not affect the run.
    pub fn open_url(&mut self, url: &str) {
        self.notice = if open::that(url).is_ok() {
            None
        } else {
            Some(format!("no way to open {url}"))
        };
    }

    /// Copy text to the clipboard, best-effort (the `open-in-*` handoff is
    /// the clipboard here is what makes the Branch chip's "click to copy" real).
    pub fn copy_text(&mut self, text: &str) {
        let copied = copy_to_clipboard(text);
        self.notice = if copied {
            Some(format!("copied {text}"))
        } else {
            Some("no clipboard tool found (wl-copy/xclip/xsel/pbcopy)".to_owned())
        };
    }

    pub fn apply_workspace_event(&mut self, event: WorkspaceEvent) {
        match event {
            WorkspaceEvent::Run { project, run } => {
                let state = self.project_tasks.entry(project.clone()).or_default();
                if let Some(existing) = state
                    .runs
                    .iter_mut()
                    .find(|existing| existing.record.id == run.record.id)
                {
                    *existing = run.clone();
                } else {
                    state.runs.push(run.clone());
                }
                if project == self.current_project() {
                    self.sync_active_project_tasks();
                }
                if self.thread_ui.data.project == project
                    && self.thread_ui.data.run_id == run.record.id
                {
                    self.thread_ui.set_run(run.clone());
                }
                let task = QuickTask::from_api(project.clone(), run.clone());
                if let Some(existing) = self
                    .quick_tasks
                    .iter_mut()
                    .find(|existing| existing.project == task.project && existing.id == task.id)
                {
                    let old_status = existing.status;
                    *existing = task.clone();
                    if let Some((summary, body)) = notification_for_transition(old_status, &task) {
                        self.pending_notifications.push((summary, body));
                    }
                } else {
                    self.quick_tasks.push(task);
                }
                self.update_global_index_for_run(&project, &run);
            }
            WorkspaceEvent::RunDeleted { project, id } => {
                self.quick_tasks
                    .retain(|task| task.project != project || task.id != id);
                if let Some(state) = self.project_tasks.get_mut(&project) {
                    state.runs.retain(|run| run.record.id != id);
                    if state
                        .selection
                        .as_ref()
                        .is_some_and(|key| key.run_id == id && key.project_id == project)
                    {
                        state.selection = None;
                    }
                }
                if project == self.current_project() {
                    self.sync_active_project_tasks();
                }
                if self.thread_ui.data.project == project && self.thread_ui.data.run_id == id {
                    self.thread_ui.clear_if_matches(&project, &id);
                    self.navigate_route(Route::Tasks {
                        project: project.clone(),
                    });
                }
                if let Some(index) = &mut self.global_index {
                    index
                        .runs
                        .retain(|entry| entry.project_id != project || entry.id != id);
                }
            }
            WorkspaceEvent::Usage { project, usage } => {
                let state = self.project_tasks.entry(project.clone()).or_default();
                state.live_usage.extend(usage.clone());
                if project == self.current_project() {
                    self.sync_active_project_tasks();
                }
                if let Some(index) = &mut self.global_index {
                    for (run_id, process_usage) in usage {
                        if let Some(entry) = index
                            .runs
                            .iter_mut()
                            .find(|entry| entry.project_id == project && entry.id == run_id)
                        {
                            entry.usage = Some(process_usage);
                        }
                    }
                }
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
            WorkspaceEvent::Lagged { count } => {
                self.record_dropped_events(count);
                self.queue_pending(PendingAction::RefreshTasks {
                    project: self.current_project().to_owned(),
                });
                self.queue_pending(PendingAction::RefreshIndex);
            }
        }
    }

    fn update_global_index_for_run(&mut self, project: &str, run: &ApiRun) {
        let Some(index) = &mut self.global_index else {
            return;
        };
        let record = &run.record;
        let Some(entry) = index
            .runs
            .iter_mut()
            .find(|entry| entry.project_id == project && entry.id == record.id)
        else {
            index.runs.push(RunIndexEntry {
                project_id: project.to_owned(),
                id: record.id.clone(),
                title: record.title.clone(),
                title_summary: record.title_summary.clone(),
                title_origin: record.title_origin,
                status: record.status,
                activity: record.activity,
                created_at: record.created_at.clone(),
                updated_at: record.updated_at.clone(),
                finished_at: record.finished_at.clone(),
                seen_at: record.seen_at.clone(),
                archived: record.archived,
                archived_at: record.archived_at.clone(),
                prompt_preview: local_prompt_preview(&record.task),
                auto_resume_at: record.auto_resume_at.clone(),
                workflow: record.workflow.clone(),
                branch: record.branch.clone(),
                started_at: record.started_at.clone(),
                pull_request_url: record.pull_request_url.clone(),
                referenced_pull_request_url: record.referenced_pull_request_url.clone(),
                pr_number: record.pr_number,
                issue_number: record.issue_number,
                referenced_issue_url: record.referenced_issue_url.clone(),
                marker_refs: record.marker_refs.clone(),
                cost_usd: record.cost_usd,
                peak_rss_bytes: record.peak_rss_bytes,
                peak_proc_count: record.peak_proc_count,
                usage: run.usage.clone(),
                runner: record.runner,
                model: record.model.clone(),
                model_usage: None,
                model_identity: record.model_identity.clone(),
                reasoning_effort: None,
            });
            return;
        };
        entry.title = record.title.clone();
        entry.title_summary = record.title_summary.clone();
        entry.title_origin = record.title_origin;
        entry.status = record.status;
        entry.activity = record.activity;
        entry.updated_at = record.updated_at.clone();
        entry.finished_at = record.finished_at.clone();
        entry.seen_at = record.seen_at.clone();
        entry.archived = record.archived;
        entry.archived_at = record.archived_at.clone();
        entry.prompt_preview = local_prompt_preview(&record.task);
        entry.auto_resume_at = record.auto_resume_at.clone();
        entry.workflow = record.workflow.clone();
        entry.branch = record.branch.clone();
        entry.started_at = record.started_at.clone();
        entry.pull_request_url = record.pull_request_url.clone();
        entry.referenced_pull_request_url = record.referenced_pull_request_url.clone();
        entry.pr_number = record.pr_number;
        entry.issue_number = record.issue_number;
        entry.referenced_issue_url = record.referenced_issue_url.clone();
        entry.marker_refs = record.marker_refs.clone();
        entry.cost_usd = record.cost_usd;
        entry.peak_rss_bytes = record.peak_rss_bytes;
        entry.peak_proc_count = record.peak_proc_count;
        entry.usage = run.usage.clone();
        entry.runner = record.runner;
        entry.model = record.model.clone();
        entry.model_identity = record.model_identity.clone();
    }

    pub fn running_count(&self) -> usize {
        self.quick_tasks
            .iter()
            .filter(|task| self.quick_task_is_visible(task))
            .filter(|task| {
                !task.archived && matches!(task.status, RunStatus::Queued | RunStatus::Running)
            })
            .count()
    }

    pub fn needs_you_count(&self) -> usize {
        self.quick_tasks
            .iter()
            .filter(|task| self.quick_task_is_visible(task))
            .filter(|task| !task.archived && task.group() == TaskGroup::NeedsYou)
            .count()
    }

    fn quick_task_is_visible(&self, task: &QuickTask) -> bool {
        matches!(self.route(), Route::GlobalTasks) || task.project == self.current_project()
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
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    || (key.kind == KeyEventKind::Repeat
                        && matches!(self.route(), Route::Scratchpad { .. })) =>
            {
                self.handle_key(key)
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) if !self.sidebar_focus => match self.route().clone() {
                Route::Terminal { .. } => {
                    crate::screens::terminal::paste(self, &text);
                }
                Route::NewTask { .. } => {
                    crate::screens::new_task::handle_paste(self, &text);
                }
                Route::Thread { .. } => {
                    crate::screens::thread::handle_paste(self, &text);
                }
                _ => {}
            },
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
        let selected = self.sidebar_selected_row();
        let mut rows: Vec<(Line<'static>, Option<HitAction>)> = Vec::new();
        rows.push((sidebar_line("  PROJECTS", self.soft_style()), None));
        for (index, project) in self.projects.iter().enumerate() {
            let marker = if project.collapsed { "+" } else { "-" };
            let mut style = if project.id == self.current_project() {
                self.active_style()
            } else {
                self.normal_style()
            };
            if selected == Some(SidebarRow::Project(index)) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            rows.push((
                sidebar_line(format!("  {marker} {}", truncate(&project.name, 20)), style),
                Some(HitAction::ProjectToggle(project.id.clone())),
            ));
            if project.id == self.current_project() && !project.collapsed {
                rows.push((
                    sidebar_nav_line(
                        "Tasks",
                        None,
                        self.route_is(NavItem::Tasks),
                        selected == Some(SidebarRow::Nav(NavItem::Tasks)),
                        self.nav_style(self.route_is(NavItem::Tasks)),
                    ),
                    Some(HitAction::Tasks),
                ));
                for nav in NavItem::ALL.into_iter().skip(1) {
                    rows.push((
                        sidebar_nav_line(
                            nav.label(),
                            None,
                            self.route_is(nav),
                            selected == Some(SidebarRow::Nav(nav)),
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
                selected == Some(SidebarRow::GlobalTasks),
                self.nav_style(matches!(self.route(), Route::GlobalTasks)),
            ),
            Some(HitAction::GlobalTasks),
        ));
        rows.push((
            sidebar_nav_line(
                "Settings",
                None,
                matches!(self.route(), Route::GlobalSettings),
                selected == Some(SidebarRow::GlobalSettings),
                self.nav_style(matches!(self.route(), Route::GlobalSettings)),
            ),
            Some(HitAction::GlobalSettings),
        ));
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
            Route::Tasks { .. } => {
                crate::screens::tasks::render(frame, area, self);
                return;
            }
            Route::GlobalTasks => {
                crate::screens::global_tasks::render(frame, area, self);
                return;
            }
            Route::GlobalSettings => {
                crate::screens::settings::render(frame, area, self);
                return;
            }
            Route::NewTask { .. } => {
                crate::screens::new_task::render(frame, area, self);
                return;
            }
            Route::Scratchpad { .. } => {
                crate::screens::scratchpad::render(frame, area, self);
                return;
            }
            Route::Thread { .. } => {
                crate::screens::thread::render(frame, area, self);
                return;
            }
            Route::TaskGit { .. } => {
                crate::screens::task_git::render(frame, area, self);
                return;
            }
            Route::Ide { .. } => {
                crate::screens::ide::render(frame, area, self);
                return;
            }
            Route::Terminal { .. } => {
                crate::screens::terminal::render(frame, area, self);
                return;
            }
            Route::Github { .. } => {
                crate::screens::github::render(frame, area, self);
                return;
            }
            Route::Skills { .. } => {
                crate::screens::skills::render(frame, area, self);
                return;
            }
            Route::Workflows { .. } => {
                crate::screens::workflows::render(frame, area, self);
                return;
            }
            Route::RepoGit { .. } => {
                crate::screens::repo_git::render(frame, area, self);
                return;
            }
            Route::Compare { .. } => {
                crate::screens::compare::render(frame, area, self);
                return;
            }
            Route::Settings { .. } => {
                crate::screens::settings::render(frame, area, self);
                return;
            }
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
            InputMode::Normal if self.uses_literal_input() => "INSERT",
            InputMode::Normal => "NORMAL",
            InputMode::Command => "COMMAND",
            InputMode::Search => "SEARCH",
        };
        let line = if self.mode == InputMode::Command {
            format!(" {mode} :{}", self.command)
        } else if self.mode == InputMode::Search {
            format!(" {mode} /{}▌", self.search)
        } else if let Some(prefix) = self.normal_input.prefix_label() {
            format!(" NORMAL  {prefix}")
        } else if let Some(toast) = &self.toast {
            format!(" {mode}  {toast}")
        } else if let Some(notice) = &self.notice {
            format!(" {mode}  {notice}")
        } else {
            let (focus, hint) = self.focus_summary();
            format!(
                " {mode}  FOCUS: {focus} — {hint}  ·  {}  {}  v0.1.0  {}  :help",
                self.current_project(),
                self.theme.name.label(),
                self.provider_summary(),
            )
        };
        let line = if self.debug_hud {
            let metrics = self.runtime_metrics;
            format!(
                " FRAME {:.1}ms  PROJ {:.1}ms  REDUCED {}  DROPPED {} ·{}",
                metrics.frame_micros as f64 / 1_000.0,
                metrics.projection_micros as f64 / 1_000.0,
                metrics.events_reduced,
                metrics.dropped_events,
                line,
            )
        } else {
            line
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

    fn render_overlays(&mut self, frame: &mut Frame<'_>, area: Rect) {
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
        } else if let Some(confirm) = self.confirm.clone() {
            self.render_confirm(frame, area, &confirm);
        } else if let Some(menu) = &self.row_menu {
            self.render_row_menu(frame, area, menu);
        } else if self.tasks_ui.sort_picker {
            self.render_sort_picker(frame, area);
        }
        if self.palette.open {
            crate::overlay::render(frame, area, self);
        }
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled(
                "NORMAL",
                Style::default().fg(self.theme.palette.accent),
            )),
            Line::from("  h/j/k/l move · gg/G first/last · Ctrl-U/D half-page"),
            Line::from("  Ctrl-W h/j/k/l window · Ctrl-W w/p next/previous"),
            Line::from("  gt/gT tab · / search · n/N match · i insert · : Ex"),
            Line::from("  Mouse capture: F12; hold Shift for terminal selection."),
            Line::from("  Esc closes this help."),
            Line::from(""),
            Line::from(Span::styled(
                "COMMANDS (type : to enter)",
                Style::default().fg(self.theme.palette.accent),
            )),
        ];
        for command in CommandId::ALL {
            lines.push(Line::from(format!(
                "  {:<28} {}",
                command.usage(),
                command.description()
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "KEYS",
            Style::default().fg(self.theme.palette.accent),
        )));
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

    fn render_confirm(&mut self, frame: &mut Frame<'_>, area: Rect, confirm: &ConfirmRequest) {
        let rect = centered_rect(area, 48.min(area.width), 7.min(area.height));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(format!("{}\n\n  [y] yes    [n] no", confirm.text))
                .block(Block::default().borders(Borders::ALL).title("CONFIRM"))
                .style(Style::default().fg(self.theme.palette.fg))
                .wrap(Wrap { trim: false }),
            rect,
        );
        let buttons_y = rect
            .y
            .saturating_add(4)
            .min(rect.bottom().saturating_sub(1));
        self.hitmap.register(
            Rect::new(rect.x.saturating_add(2), buttons_y, 9, 1),
            20,
            HitAction::ConfirmYes,
        );
        self.hitmap.register(
            Rect::new(rect.x.saturating_add(13), buttons_y, 8, 1),
            20,
            HitAction::ConfirmNo,
        );
    }

    fn render_row_menu(&self, frame: &mut Frame<'_>, area: Rect, menu: &RowMenu) {
        let height = (menu.items.len() as u16 + 3).min(area.height.saturating_sub(2));
        let width = 30.min(area.width);
        let rect = centered_rect(area, width, height);
        frame.render_widget(Clear, rect);
        let mut lines = vec![Line::from(Span::styled(
            format!(" {}", menu.title),
            Style::default().fg(self.theme.palette.soft_fg),
        ))];
        for (index, item) in menu.items.iter().enumerate() {
            let selected = index == menu.selected;
            let style = if selected {
                Style::default()
                    .fg(self.theme.palette.accent)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(self.theme.palette.fg)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    " {}  {}{}",
                    if selected { ">" } else { " " },
                    item.label,
                    if selected { " <" } else { "" }
                ),
                style,
            )));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title("ACTIONS"))
                .style(Style::default().bg(self.theme.palette.surface))
                .wrap(Wrap { trim: false }),
            rect,
        );
    }

    fn render_sort_picker(&self, frame: &mut Frame<'_>, area: Rect) {
        let items: [(&str, ColumnId); 5] = [
            ("Status", ColumnId::Status),
            ("Started", ColumnId::Started),
            ("Tokens", ColumnId::Tokens),
            ("Cost", ColumnId::Cost),
            ("Workflow", ColumnId::Workflow),
        ];
        let height = (items.len() as u16 + 2).min(area.height.saturating_sub(2));
        let width = 22.min(area.width);
        let rect = centered_rect(area, width, height);
        frame.render_widget(Clear, rect);
        let lines: Vec<Line<'static>> = items
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let selected = index == self.sort_picker_index;
                let style = if selected {
                    Style::default()
                        .fg(self.theme.palette.accent)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(self.theme.palette.fg)
                };
                Line::from(Span::styled(
                    format!(" {}  {label}", if selected { ">" } else { " " }),
                    style,
                ))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title("SORT BY"))
                .style(Style::default().bg(self.theme.palette.surface)),
            rect,
        );
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        self.hover = Some((mouse.column, mouse.row));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = self.hitmap.hit(mouse.column, mouse.row) {
                    let sidebar_action = self.sidebar_row_for_hit(&action).is_some();
                    if let Some(row) = self.sidebar_row_for_hit(&action)
                        && let Some(index) = self.sidebar_position(row)
                    {
                        self.sidebar_selected = index;
                        self.sidebar_focus = true;
                    }
                    // Press on a workflow step arms the drag-reorder; the click itself is
                    // still applied (it moves the step cursor), the release does the move.
                    if let HitAction::WorkflowStep(index) = action
                        && matches!(self.route(), Route::Workflows { .. })
                    {
                        self.workflows_ui.drag_source = Some(index);
                    }
                    if action == HitAction::SidebarEdge {
                        self.sidebar_dragging = true;
                    } else {
                        self.apply_hit_action(action);
                        if sidebar_action {
                            self.sidebar_focus = true;
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(HitAction::TableRow(index)) = self.hitmap.hit(mouse.column, mouse.row) {
                    match self.route() {
                        Route::Tasks { .. } => {
                            self.tasks_ui.table.select(Some(index));
                            crate::screens::tasks::open_row_menu(self);
                        }
                        Route::GlobalTasks => {
                            self.global_ui.table.select(Some(index));
                            crate::screens::global_tasks::open_row_menu(self);
                        }
                        _ => {}
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.sidebar_dragging => {
                self.sidebar_width = mouse
                    .column
                    .saturating_add(1)
                    .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(source) = self.workflows_ui.drag_source.take()
                    && let Some(HitAction::WorkflowStep(target)) =
                        self.hitmap.hit(mouse.column, mouse.row)
                    && matches!(self.route(), Route::Workflows { .. })
                {
                    crate::screens::workflows::drop_step(self, source, target);
                }
                self.sidebar_dragging = false;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if !self.sidebar_focus
                    && matches!(self.route(), Route::Thread { .. })
                    && self.thread_ui.focus == crate::screens::thread::ThreadFocus::Transcript
                    && self
                        .thread_ui
                        .transcript_area
                        .is_some_and(|area| area.contains((mouse.column, mouse.row).into())) =>
            {
                crate::screens::thread::handle_scroll(
                    self,
                    matches!(mouse.kind, MouseEventKind::ScrollUp),
                );
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if !self.sidebar_focus
                    && matches!(self.route(), Route::Terminal { .. })
                    && self
                        .terminal_ui
                        .last_area
                        .is_some_and(|area| area.contains((mouse.column, mouse.row).into())) =>
            {
                crate::screens::terminal::scroll(
                    self,
                    matches!(mouse.kind, MouseEventKind::ScrollUp),
                );
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.palette.open {
            crate::overlay::handle_key(self, key);
            return;
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help_open = false;
            }
            return;
        }
        if let Some(confirm) = self.confirm.clone() {
            self.handle_confirm_key(&confirm, key);
            return;
        }
        if let Some(menu) = self.row_menu.clone()
            && handle_row_menu_key(self, &menu, key)
        {
            return;
        }
        if self.mode == InputMode::Command {
            self.handle_command_key(key);
            return;
        }
        if self.mode == InputMode::Search {
            self.handle_search_key(key);
            return;
        }
        let starts_window_prefix = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('w');
        if self.normal_input.prefix_label().is_some()
            || !self.uses_literal_input()
            || starts_window_prefix
        {
            match self.normal_input.feed(key) {
                FeedResult::Pending | FeedResult::Cancelled => return,
                FeedResult::Command(command) => {
                    self.apply_normal_command(command);
                    return;
                }
                FeedResult::Pass => {}
            }
        }
        if self.sidebar_focus {
            if self.handle_sidebar_key(key) {
                return;
            }
            if let Some(action) = self.keymap.action_for(KeyMode::Normal, &key) {
                self.apply_action(action);
            }
            return;
        }
        if self.handle_route_key(key) {
            return;
        }
        if let Some(action) = self.keymap.action_for(KeyMode::Normal, &key) {
            self.apply_action(action);
            return;
        }
        if key.code == KeyCode::Esc && self.sidebar_overlay_open {
            self.sidebar_overlay_open = false;
        }
    }

    fn handle_route_key(&mut self, key: KeyEvent) -> bool {
        match self.route().clone() {
            Route::Tasks { .. } => crate::screens::tasks::handle_key(self, key),
            Route::GlobalTasks => crate::screens::global_tasks::handle_key(self, key),
            Route::GlobalSettings => crate::screens::settings::handle_key(self, key),
            Route::NewTask { .. } => crate::screens::new_task::handle_key(self, key),
            Route::Scratchpad { .. } => crate::screens::scratchpad::handle_key(self, key),
            Route::Thread { .. } => crate::screens::thread::handle_key(self, key),
            Route::TaskGit { .. } => crate::screens::task_git::handle_key(self, key),
            Route::Ide { .. } => crate::screens::ide::handle_key(self, key),
            Route::Terminal { .. } => crate::screens::terminal::handle_key(self, key),
            Route::Github { .. } => crate::screens::github::handle_key(self, key),
            Route::Skills { .. } => crate::screens::skills::handle_key(self, key),
            Route::Workflows { .. } => crate::screens::workflows::handle_key(self, key),
            Route::RepoGit { .. } => crate::screens::repo_git::handle_key(self, key),
            Route::Compare { .. } => crate::screens::compare::handle_key(self, key),
            Route::Settings { .. } => crate::screens::settings::handle_key(self, key),
            Route::Placeholder { .. } => false,
        }
    }

    fn uses_literal_input(&self) -> bool {
        if self.sidebar_focus {
            return false;
        }
        match self.route() {
            Route::NewTask { .. } => {
                self.new_task_ui.composer_focused || self.new_task_ui.picker.is_some()
            }
            Route::Scratchpad { .. } | Route::Terminal { .. } => true,
            Route::Thread { .. } => {
                self.thread_ui.subagent_sheet.is_some()
                    || matches!(
                        self.thread_ui.focus,
                        crate::screens::thread::ThreadFocus::Composer
                            | crate::screens::thread::ThreadFocus::ReviewNotes
                    )
            }
            Route::Ide { .. } => self.ide_ui.focus == crate::screens::ide::IdeFocus::Editor,
            Route::Github { .. } => {
                self.github_ui.focus == crate::screens::github::GithubFocus::SkillPicker
            }
            Route::Workflows { .. } => self.workflows_ui.import_open || self.workflows_ui.name_open,
            Route::RepoGit { .. } => self.repo_git_ui.new_branch_open,
            Route::TaskGit { .. } => self.task_git_ui.commit_dialog_open,
            Route::Settings { .. } | Route::GlobalSettings => {
                self.settings_ui.file_editing || self.settings_ui.edit.is_some()
            }
            _ => false,
        }
    }

    fn apply_normal_command(&mut self, command: NormalCommand) {
        match command {
            NormalCommand::Motion(direction) => {
                let code = match direction {
                    VimDirection::Left => KeyCode::Char('h'),
                    VimDirection::Down => KeyCode::Char('j'),
                    VimDirection::Up => KeyCode::Char('k'),
                    VimDirection::Right => KeyCode::Char('l'),
                };
                let key = KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
                if self.sidebar_focus {
                    self.handle_sidebar_key(key);
                } else {
                    self.handle_route_key(key);
                }
            }
            NormalCommand::Window(direction) => self.focus_window(direction),
            NormalCommand::WindowNext => self.cycle_window(),
            NormalCommand::WindowPrevious => self.restore_previous_window(),
            NormalCommand::First => self.jump_to_boundary(false),
            NormalCommand::Last => self.jump_to_boundary(true),
            NormalCommand::HalfPageUp => self.half_page(false),
            NormalCommand::HalfPageDown => self.half_page(true),
            NormalCommand::NextTab => self.cycle_task_tab(true),
            NormalCommand::PreviousTab => self.cycle_task_tab(false),
            NormalCommand::Search => self.begin_search(),
            NormalCommand::SearchNext => self.repeat_search(true),
            NormalCommand::SearchPrevious => self.repeat_search(false),
            NormalCommand::Insert => {
                let key = KeyEvent::new(KeyCode::Char('i'), crossterm::event::KeyModifiers::NONE);
                self.handle_route_key(key);
            }
            NormalCommand::Ex => {
                self.mode = InputMode::Command;
                self.command.clear();
                self.notice = None;
            }
        }
    }

    fn begin_search(&mut self) {
        self.mode = InputMode::Search;
        self.search.clear();
        self.notice = None;
        self.apply_search_query();
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.search.clear();
            }
            KeyCode::Enter => {
                self.mode = InputMode::Normal;
                if !self.search.is_empty() {
                    self.last_search = self.search.clone();
                    self.repeat_search(true);
                }
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.apply_search_query();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.search.push(character);
                self.apply_search_query();
            }
            _ => {}
        }
    }

    fn apply_search_query(&mut self) {
        let query = self.search.clone();
        match self.route() {
            Route::Tasks { .. } => self.tasks_ui.query = query,
            Route::GlobalTasks => self.global_ui.query = query,
            Route::Skills { .. } => {
                self.skills_ui.query = query;
                self.skills_ui.selected = 0;
            }
            Route::Workflows { .. } => {
                self.workflows_ui.focus = crate::screens::workflows::WorkflowFocus::Palette;
                self.workflows_ui.palette_query = query;
                self.workflows_ui.palette_selected = 0;
            }
            Route::Thread { .. } => {}
            _ => {
                self.notice = Some("search is not available in this view".to_owned());
            }
        }
    }

    fn repeat_search(&mut self, forward: bool) {
        if self.last_search.is_empty() {
            return;
        }
        match self.route() {
            Route::Tasks { .. } => {
                self.tasks_ui
                    .table
                    .cycle_selection(if forward { 1 } else { -1 })
            }
            Route::GlobalTasks => {
                self.global_ui
                    .table
                    .cycle_selection(if forward { 1 } else { -1 })
            }
            Route::Skills { .. } => {
                crate::screens::skills::move_search_match(self, forward);
            }
            Route::Workflows { .. } => {
                crate::screens::workflows::move_palette_selection(
                    self,
                    if forward { 1 } else { -1 },
                );
            }
            Route::Thread { .. } => {
                let area = self.thread_ui.transcript_area.unwrap_or_default();
                self.thread_ui.transcript.select_next_match(
                    &self.last_search,
                    if forward { 1 } else { -1 },
                    area.width,
                    area.height,
                );
            }
            _ => {}
        }
    }

    fn jump_to_boundary(&mut self, end: bool) {
        if self.sidebar_focus {
            let len = self.sidebar_rows().len();
            self.sidebar_selected = if end { len.saturating_sub(1) } else { 0 };
            return;
        }
        match self.route() {
            Route::Tasks { .. } => {
                let index = end.then(|| self.tasks_ui.table.rows.len().saturating_sub(1));
                self.tasks_ui.table.select(index.or(Some(0)));
            }
            Route::GlobalTasks => {
                let index = end.then(|| self.global_ui.table.rows.len().saturating_sub(1));
                self.global_ui.table.select(index.or(Some(0)));
            }
            Route::Thread { .. } if end => self.thread_ui.transcript.jump_to_bottom(),
            Route::Thread { .. } => self.thread_ui.transcript.jump_to_top(),
            Route::Skills { .. } => {
                let query = self.skills_ui.query.to_lowercase();
                self.skills_ui.selected = if end {
                    self.skills_ui
                        .skills
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(index, skill)| {
                            (query.is_empty()
                                || skill.name.to_lowercase().contains(&query)
                                || skill
                                    .description
                                    .as_deref()
                                    .is_some_and(|value| value.to_lowercase().contains(&query)))
                            .then_some(index)
                        })
                        .unwrap_or(0)
                } else {
                    0
                };
            }
            Route::Workflows { .. } => match self.workflows_ui.focus {
                crate::screens::workflows::WorkflowFocus::Palette => {
                    let query = self.workflows_ui.palette_query.to_lowercase();
                    self.workflows_ui.palette_selected = if end {
                        self.workflows_ui
                            .palette_skills
                            .iter()
                            .enumerate()
                            .rev()
                            .find_map(|(index, skill)| {
                                (query.is_empty()
                                    || skill.name.to_lowercase().contains(&query)
                                    || skill
                                        .description
                                        .as_deref()
                                        .is_some_and(|value| value.to_lowercase().contains(&query)))
                                .then_some(index)
                            })
                            .unwrap_or(0)
                    } else {
                        0
                    };
                }
                _ => {
                    let count =
                        if self.workflows_ui.selected_tab < self.workflows_ui.workflows.len() {
                            self.workflows_ui.workflows[self.workflows_ui.selected_tab]
                                .steps
                                .len()
                        } else {
                            self.workflows_ui.draft_steps.len()
                        };
                    self.workflows_ui.steps_selected =
                        if end { count.saturating_sub(1) } else { 0 };
                }
            },
            Route::Github { .. } => {
                if self.github_ui.focus == crate::screens::github::GithubFocus::Detail {
                    if end {
                        match self.github_ui.detail_tab {
                            crate::screens::github::GithubDetailTab::Thread => {
                                self.github_ui.comments_scroll = usize::MAX
                            }
                            crate::screens::github::GithubDetailTab::Changes => {
                                self.github_ui.changes_scroll = usize::MAX
                            }
                        }
                    } else {
                        self.github_ui.comments_scroll = 0;
                        self.github_ui.changes_scroll = 0;
                    }
                } else {
                    let count =
                        self.github_ui
                            .data
                            .as_ref()
                            .map_or(0, |data| match self.github_ui.tab {
                                crate::screens::github::GithubTab::Issues => data.issues.len(),
                                crate::screens::github::GithubTab::Prs => data.prs.len(),
                            });
                    self.github_ui.list_selected = if end { count.saturating_sub(1) } else { 0 };
                }
            }
            Route::RepoGit { .. } => crate::screens::repo_git::jump_selection(self, end),
            Route::TaskGit { .. } => crate::screens::task_git::jump_selection(self, end),
            Route::Ide { .. } => crate::screens::ide::jump_tree(self, end),
            Route::Compare { .. } => crate::screens::compare::jump_selection(self, end),
            Route::Settings { .. } | Route::GlobalSettings => {
                crate::screens::settings::jump_selection(self, end)
            }
            _ => {}
        }
    }

    fn half_page(&mut self, down: bool) {
        let code = if down { KeyCode::Down } else { KeyCode::Up };
        let key = KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        for _ in 0..10 {
            if self.sidebar_focus {
                self.handle_sidebar_key(key);
            } else {
                self.handle_route_key(key);
            }
        }
    }

    fn cycle_task_tab(&mut self, forward: bool) {
        let delta = if forward { 1 } else { -1 };
        match self.route().clone() {
            Route::Thread { project, id } => {
                let tab = if forward {
                    TaskGitTab::Changes
                } else {
                    TaskGitTab::Commits
                };
                crate::screens::task_git::open(self, &project, &id, tab);
            }
            Route::TaskGit { .. } => crate::screens::task_git::switch_tab(self, delta),
            Route::RepoGit { project, tab } => {
                let order = [
                    RepoGitTab::Commits,
                    RepoGitTab::Changes,
                    RepoGitTab::Branches,
                ];
                let current = order
                    .iter()
                    .position(|candidate| *candidate == tab)
                    .unwrap_or(0);
                let next = (current as i32 + delta).rem_euclid(order.len() as i32) as usize;
                crate::screens::repo_git::open(self, &project, order[next]);
            }
            Route::Github { .. } => {
                use crate::input::hitmap::GithubAction;
                use crate::screens::github::{GithubDetailTab, GithubTab};
                if self.github_ui.focus == crate::screens::github::GithubFocus::Detail
                    && self.github_ui.detail_item.is_some()
                {
                    let next = match self.github_ui.detail_tab {
                        GithubDetailTab::Thread => GithubDetailTab::Changes,
                        GithubDetailTab::Changes => GithubDetailTab::Thread,
                    };
                    crate::screens::github::apply_hit(self, GithubAction::SwitchDetailTab(next));
                } else {
                    let next = match self.github_ui.tab {
                        GithubTab::Issues => GithubTab::Prs,
                        GithubTab::Prs => GithubTab::Issues,
                    };
                    crate::screens::github::apply_hit(self, GithubAction::SwitchTab(next));
                }
            }
            Route::Workflows { .. } => {
                let count = self.workflows_ui.workflows.len() + 1;
                let next = (self.workflows_ui.selected_tab as i32 + delta).rem_euclid(count as i32)
                    as usize;
                crate::screens::workflows::apply_tab_hit(self, next);
            }
            Route::Compare { .. } => {
                let code = if forward {
                    KeyCode::Right
                } else {
                    KeyCode::Left
                };
                self.handle_route_key(KeyEvent::new(code, crossterm::event::KeyModifiers::NONE));
            }
            _ => self.notice = Some("this view has no tabs".to_owned()),
        }
    }

    fn handle_confirm_key(&mut self, confirm: &ConfirmRequest, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let action = confirm.action.clone();
                self.confirm = None;
                if let PendingAction::CancelRun { project, id } = &action
                    && self.thread_ui.data.project == *project
                    && self.thread_ui.data.run_id == *id
                {
                    self.thread_ui.cancel_pending = true;
                }
                match action {
                    // Quit, scratchpad clearing, and IDE-discard resolutions are app-local state changes,
                    // resolved here; everything else waits for main's engine.
                    PendingAction::Quit => self.quit = true,
                    PendingAction::ClearScratchpad { project } => {
                        crate::screens::scratchpad::clear_after_confirmation(self, &project);
                    }
                    PendingAction::IdeDiscardThenNavigate(route) => {
                        self.ide_ui.discard();
                        self.navigate_route(*route);
                    }
                    PendingAction::IdeDiscardThenBack => {
                        self.ide_ui.discard();
                        self.go_back();
                    }
                    PendingAction::IdeDiscardThenForward => {
                        self.ide_ui.discard();
                        self.go_forward();
                    }
                    PendingAction::SwitchProject(project) => {
                        self.ide_ui.discard();
                        self.apply_project_switch(project);
                    }
                    other => self.pending.push(other),
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => self.confirm = None,
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

    pub(crate) fn execute_command(&mut self, command: &str) {
        let mut parts = command.split_whitespace();
        let Some(name) = parts.next() else {
            return;
        };
        let Some(command) = CommandId::parse(name) else {
            self.notice = Some(format!("unknown command: {name}"));
            return;
        };
        match command {
            CommandId::Open => {
                if let Some(path) = parts.next() {
                    match Route::parse(path, self.default_project.as_str()) {
                        Some(Route::GlobalSettings) => crate::screens::settings::open_global(self),
                        Some(Route::Settings { project }) => {
                            crate::screens::settings::open(self, &project)
                        }
                        Some(route) => self.request_navigate(route),
                        None => self.notice = Some(format!("unknown route: {path}")),
                    }
                } else {
                    self.notice = Some("usage: :open <route>".to_owned());
                }
            }
            CommandId::Back => {
                self.request_back();
            }
            CommandId::Forward => {
                self.request_forward();
            }
            CommandId::Theme => {
                if let Some(name) = parts.next().and_then(ThemeName::parse) {
                    self.theme = Theme::new(name, self.theme.capability);
                    let mut appearance = self
                        .settings_ui
                        .workspace_ui_state
                        .as_ref()
                        .and_then(|state| state.appearance.clone())
                        .unwrap_or_default();
                    appearance.theme = Some(match name {
                        ThemeName::Dark => coducktor_contract::ThemePreference::Dark,
                        ThemeName::LazyVim => coducktor_contract::ThemePreference::Lazyvim,
                        ThemeName::Lakes => coducktor_contract::ThemePreference::Lakes,
                    });
                    self.pending
                        .push(PendingAction::SettingsPutWorkspaceUiState {
                            input: coducktor_contract::WorkspaceUiState {
                                appearance: Some(appearance),
                                ..Default::default()
                            },
                        });
                } else {
                    self.notice = Some("theme must be dark, lazyvim, or lakes".to_owned());
                }
            }
            CommandId::New => self.navigate(NavItem::NewTask),
            CommandId::ClearScratchpad => crate::screens::scratchpad::request_clear(self),
            CommandId::Help => self.help_open = true,
            CommandId::Sidebar => self.toggle_sidebar(),
            CommandId::Stop => self.apply_thread_command(
                crate::screens::thread::ThreadAction::Cancel,
                ":stop requires an open task session",
            ),
            CommandId::Finish => self.apply_thread_command(
                crate::screens::thread::ThreadAction::Finish,
                ":finish requires an open task session",
            ),
            CommandId::Archive => self.apply_thread_command(
                crate::screens::thread::ThreadAction::Archive,
                ":archive requires an open task session",
            ),
            CommandId::Delete => self.apply_thread_command(
                crate::screens::thread::ThreadAction::Delete,
                ":delete requires an open task session or removable settings row",
            ),
            CommandId::Quit => self.request_quit(),
        }
    }

    fn apply_thread_command(
        &mut self,
        action: crate::screens::thread::ThreadAction,
        unavailable: &str,
    ) {
        if action == crate::screens::thread::ThreadAction::Delete
            && matches!(self.route(), Route::Settings { .. } | Route::GlobalSettings)
        {
            crate::screens::settings::delete_selected(self);
            return;
        }
        if !matches!(self.route(), Route::Thread { .. } | Route::TaskGit { .. }) {
            self.notice = Some(unavailable.to_owned());
            return;
        }
        let Some(run) = self.thread_ui.data.run.as_ref() else {
            self.notice = Some(unavailable.to_owned());
            return;
        };
        let flags = crate::screens::thread::actions::run_action_flags(run);
        let allowed = match &action {
            crate::screens::thread::ThreadAction::Cancel => flags.cancel,
            crate::screens::thread::ThreadAction::Finish => flags.finish,
            crate::screens::thread::ThreadAction::Archive => flags.archive,
            crate::screens::thread::ThreadAction::Delete => flags.delete_run,
            _ => false,
        };
        if !allowed {
            self.notice = Some(format!(
                "{} is not available for this task",
                action.command_name()
            ));
            return;
        }
        crate::screens::thread::apply_hit(self, action);
    }

    fn apply_action(&mut self, action: ActionId) {
        match action {
            ActionId::Quit => self.request_quit(),
            ActionId::Tasks => self.navigate(NavItem::Tasks),
            ActionId::GlobalTasks => {
                self.request_navigate(Route::GlobalTasks);
                self.queue_pending(PendingAction::RefreshIndex);
            }
            ActionId::NewTask => self.navigate(NavItem::NewTask),
            ActionId::Ide => self.navigate(NavItem::Ide),
            ActionId::RepoGit => self.navigate(NavItem::RepoGit),
            ActionId::Github => self.navigate(NavItem::Github),
            ActionId::Skills => self.navigate(NavItem::Skills),
            ActionId::Workflows => self.navigate(NavItem::Workflows),
            ActionId::Settings => self.navigate(NavItem::Settings),
            ActionId::ToggleSidebar => self.toggle_sidebar(),
            ActionId::FocusSidebar => self.focus_sidebar(),
            ActionId::Help => self.help_open = true,
            ActionId::Palette => crate::overlay::open(self),
            ActionId::Back => {
                self.request_back();
            }
            ActionId::Forward => {
                self.request_forward();
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
            HitAction::GlobalTasks => {
                self.request_navigate(Route::GlobalTasks);
                self.queue_pending(PendingAction::RefreshIndex);
            }
            HitAction::GlobalSettings => crate::screens::settings::open_global(self),
            HitAction::NewTask => self.navigate(NavItem::NewTask),
            HitAction::Scratchpad => self.navigate(NavItem::Scratchpad),
            HitAction::Ide => self.navigate(NavItem::Ide),
            HitAction::Terminal => self.navigate(NavItem::Terminal),
            HitAction::RepoGit => self.navigate(NavItem::RepoGit),
            HitAction::Github => self.navigate(NavItem::Github),
            HitAction::Skills => self.navigate(NavItem::Skills),
            HitAction::Workflows => self.navigate(NavItem::Workflows),
            HitAction::Settings => self.navigate(NavItem::Settings),
            HitAction::ActiveTasks => self.set_task_filter(TaskFilter::Active),
            HitAction::ArchivedTasks => self.set_task_filter(TaskFilter::Archived),
            HitAction::ToggleSidebar => self.toggle_sidebar(),
            HitAction::Help => self.help_open = true,
            HitAction::ProjectToggle(project) => self.select_project(project),
            HitAction::SidebarEdge => self.sidebar_dragging = true,
            HitAction::Back => {
                self.request_back();
            }
            HitAction::Forward => {
                self.request_forward();
            }
            HitAction::Quit => self.request_quit(),
            HitAction::ConfirmYes => {
                if let Some(confirm) = self.confirm.clone() {
                    self.handle_confirm_key(
                        &confirm,
                        KeyEvent::new(KeyCode::Char('y'), crossterm::event::KeyModifiers::NONE),
                    );
                }
            }
            HitAction::ConfirmNo => self.confirm = None,
            HitAction::TableHeader(column) => {
                if let Route::Tasks { .. } = self.route() {
                    crate::screens::tasks::handle_table_hit(self, HitAction::TableHeader(column));
                }
            }
            HitAction::TableRow(index) => match self.route() {
                Route::Tasks { .. } => {
                    self.tasks_ui.table.select(Some(index));
                    crate::screens::tasks::handle_table_hit(self, HitAction::TableRow(index));
                }
                Route::GlobalTasks => {
                    self.global_ui.table.select(Some(index));
                    let Some((_, row)) = self.global_ui.table.selected_row() else {
                        return;
                    };
                    let key = row.key.clone();
                    crate::screens::global_tasks::open_thread(self, &key);
                }
                _ => {}
            },
            HitAction::PickerRow(index) => {
                if matches!(self.route(), Route::NewTask { .. }) {
                    crate::screens::new_task::pick_index(self, index);
                } else if matches!(self.route(), Route::Settings { .. } | Route::GlobalSettings) {
                    crate::screens::settings::pick_model_index(self, index);
                }
            }
            HitAction::ComposerRemoveAttachment(index) => {
                if matches!(self.route(), Route::NewTask { .. }) {
                    crate::screens::new_task::remove_attachment(self, index);
                } else if matches!(self.route(), Route::Thread { .. }) {
                    self.thread_ui.composer.remove_attachment(index);
                }
            }
            HitAction::NewTaskScreen(action) => {
                if matches!(self.route(), Route::NewTask { .. }) {
                    crate::screens::new_task::apply_hit(self, action);
                }
            }
            HitAction::ThreadScreen(action) => {
                if matches!(self.route(), Route::Thread { .. }) {
                    crate::screens::thread::apply_hit(self, action);
                }
            }
            HitAction::TaskGitScreen(action) => {
                if matches!(self.route(), Route::TaskGit { .. }) {
                    crate::screens::task_git::apply_hit(self, action);
                }
            }
            HitAction::IdeScreen(action) => {
                if matches!(self.route(), Route::Ide { .. }) {
                    crate::screens::ide::apply_hit(self, action);
                }
            }
            HitAction::GithubScreen(action) => {
                if matches!(self.route(), Route::Github { .. }) {
                    crate::screens::github::apply_hit(self, action);
                }
            }
            HitAction::SkillsScreen(index) => {
                if matches!(self.route(), Route::Skills { .. }) {
                    self.skills_ui.selected = index;
                }
            }
            HitAction::WorkflowTab(index) => {
                if matches!(self.route(), Route::Workflows { .. }) {
                    crate::screens::workflows::apply_tab_hit(self, index);
                }
            }
            HitAction::WorkflowStep(index) => {
                if matches!(self.route(), Route::Workflows { .. }) {
                    crate::screens::workflows::apply_step_hit(self, index);
                }
            }
            HitAction::WorkflowSkill(index) => {
                if matches!(self.route(), Route::Workflows { .. }) {
                    crate::screens::workflows::apply_palette_hit(self, index);
                }
            }
            HitAction::WorkflowControl(action) => {
                if matches!(self.route(), Route::Workflows { .. }) {
                    crate::screens::workflows::apply_control(self, action);
                }
            }
            HitAction::RepoGitScreen(action) => {
                if matches!(self.route(), Route::RepoGit { .. }) {
                    crate::screens::repo_git::apply_hit(self, action);
                }
            }
            HitAction::CompareScreen(action) => {
                if matches!(self.route(), Route::Compare { .. }) {
                    crate::screens::compare::apply_hit(self, action);
                }
            }
            HitAction::SettingsSection(index) => {
                if matches!(self.route(), Route::Settings { .. } | Route::GlobalSettings) {
                    self.settings_ui.section = index;
                    self.settings_ui.row = 0;
                }
            }
            HitAction::SettingsRow(index) => {
                if matches!(self.route(), Route::Settings { .. } | Route::GlobalSettings) {
                    self.settings_ui.row = index;
                    crate::screens::settings::activate_selected(self);
                }
            }
            HitAction::SettingsDeleteRow(index) => {
                if matches!(self.route(), Route::Settings { .. } | Route::GlobalSettings) {
                    self.settings_ui.row = index;
                    crate::screens::settings::delete_selected(self);
                }
            }
            HitAction::OpenCompare(group_id) => {
                let project = self.current_project().to_owned();
                crate::screens::compare::open(self, &project, &group_id);
            }
        }
    }

    pub(crate) fn navigate(&mut self, nav: NavItem) {
        let project = self.current_project().to_owned();
        match nav {
            NavItem::Tasks => {
                self.request_navigate(Route::Tasks {
                    project: project.clone(),
                });
                self.queue_pending(PendingAction::RefreshTasks { project });
            }
            NavItem::NewTask => {
                self.request_navigate(Route::NewTask {
                    project: project.clone(),
                });
                self.queue_pending(PendingAction::RefreshNewTask { project });
                // The hero auto-focuses the composer.
                self.new_task_ui.composer_focused = true;
                self.new_task_ui.composer.focus();
            }
            NavItem::Scratchpad => crate::screens::scratchpad::open(self, &project),
            NavItem::Ide => crate::screens::ide::open(self, &project),
            NavItem::Terminal => crate::screens::terminal::open(self, &project),
            NavItem::Github => crate::screens::github::open(self, &project),
            NavItem::Skills => crate::screens::skills::open(self, &project),
            NavItem::Workflows => crate::screens::workflows::open(self, &project),
            NavItem::RepoGit => crate::screens::repo_git::open(self, &project, RepoGitTab::Commits),
            NavItem::Settings => crate::screens::settings::open(self, &project),
        }
        self.notice = None;
    }

    /// Select a project from the sidebar. Clicking the active project's row retains its old
    /// collapse behavior; clicking another project opens its Tasks route and refreshes the
    /// project-scoped data before the next frame.
    fn select_project(&mut self, project: String) {
        if project == self.current_project() && self.route().project().is_some() {
            if let Some(entry) = self.projects.iter_mut().find(|entry| entry.id == project) {
                entry.collapsed = !entry.collapsed;
                if entry.collapsed
                    && let Some(index) = self.projects.iter().position(|entry| entry.id == project)
                {
                    self.sidebar_selected = index;
                }
            }
            return;
        }
        self.switch_project(project);
    }

    /// Switch projects from the command palette or another non-sidebar caller.
    pub(crate) fn switch_project(&mut self, project: String) {
        if self.ide_ui.dirty {
            self.confirm = Some(ConfirmRequest {
                text: "Discard unsaved changes and switch projects?".to_owned(),
                action: PendingAction::SwitchProject(project),
            });
        } else {
            self.apply_project_switch(project);
        }
    }

    fn apply_project_switch(&mut self, project: String) {
        self.persist_active_task_ui();
        self.default_project = project.clone();
        // The switch is the resolution of the IDE-dirty guard, so navigate
        // directly here and keep the selector on the new project's row.
        self.history.navigate(Route::Tasks {
            project: project.clone(),
        });
        self.sync_active_project_tasks();
        self.screen_focus = 0;
        if let Some(index) = self.projects.iter().position(|entry| entry.id == project) {
            self.sidebar_selected = index;
        }
        self.queue_pending(PendingAction::RefreshTasks {
            project: project.clone(),
        });
        self.queue_pending(PendingAction::RefreshNewTask { project });
        self.notice = None;
    }

    fn route_is(&self, nav: NavItem) -> bool {
        matches!(self.route(), Route::Placeholder { nav: current, .. } if *current == nav)
            || (nav == NavItem::Tasks
                && matches!(self.route(), Route::Tasks { .. } | Route::Thread { .. }))
            || (nav == NavItem::NewTask && matches!(self.route(), Route::NewTask { .. }))
            || (nav == NavItem::Scratchpad && matches!(self.route(), Route::Scratchpad { .. }))
            || (nav == NavItem::Ide && matches!(self.route(), Route::Ide { .. }))
            || (nav == NavItem::Terminal && matches!(self.route(), Route::Terminal { .. }))
            || (nav == NavItem::Github && matches!(self.route(), Route::Github { .. }))
            || (nav == NavItem::Skills && matches!(self.route(), Route::Skills { .. }))
            || (nav == NavItem::Workflows && matches!(self.route(), Route::Workflows { .. }))
            || (nav == NavItem::RepoGit && matches!(self.route(), Route::RepoGit { .. }))
            || (nav == NavItem::Settings && matches!(self.route(), Route::Settings { .. }))
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar_focus = false;
        if self.last_width == 0 || self.last_width < SIDEBAR_BREAKPOINT {
            self.sidebar_overlay_open = !self.sidebar_overlay_open;
        } else {
            self.sidebar_collapsed = !self.sidebar_collapsed;
        }
    }

    pub fn focus_sidebar(&mut self) {
        if self.last_width != 0 && self.last_width < SIDEBAR_BREAKPOINT {
            self.sidebar_overlay_open = true;
        } else if self.last_width >= SIDEBAR_BREAKPOINT {
            self.sidebar_collapsed = false;
        }
        self.sidebar_focus = true;
    }

    fn screen_pane_count(&self) -> usize {
        match self.route() {
            Route::Ide { .. }
            | Route::Workflows { .. }
            | Route::Github { .. }
            | Route::Skills { .. } => 2,
            Route::Settings { .. } | Route::GlobalSettings => {
                if self.settings_ui.file_editing {
                    1
                } else {
                    2
                }
            }
            Route::RepoGit { tab, .. } => match tab {
                RepoGitTab::Changes | RepoGitTab::Commits => 2,
                RepoGitTab::Branches => 1,
            },
            Route::TaskGit { tab, .. } => match tab {
                TaskGitTab::Changes | TaskGitTab::Commits => 2,
                TaskGitTab::Files => 1,
            },
            _ => 1,
        }
    }

    fn current_screen_pane(&self) -> usize {
        let max = self.screen_pane_count().saturating_sub(1);
        let pane = match self.route() {
            Route::Ide { .. } => match self.ide_ui.focus {
                crate::screens::ide::IdeFocus::Tree => 0,
                crate::screens::ide::IdeFocus::Editor => 1,
            },
            Route::Workflows { .. } => match self.workflows_ui.focus {
                crate::screens::workflows::WorkflowFocus::Palette => 0,
                crate::screens::workflows::WorkflowFocus::Steps
                | crate::screens::workflows::WorkflowFocus::Tabs => 1,
            },
            Route::TaskGit { tab, .. } => match tab {
                TaskGitTab::Changes | TaskGitTab::Commits => match self.task_git_ui.focus {
                    crate::screens::task_git::TaskGitFocus::Tree => 0,
                    crate::screens::task_git::TaskGitFocus::Diff => 1,
                },
                TaskGitTab::Files => 0,
            },
            Route::Github { .. } => match self.github_ui.focus {
                crate::screens::github::GithubFocus::Detail => 1,
                _ => 0,
            },
            _ => self.screen_focus,
        };
        pane.min(max)
    }

    fn set_screen_pane(&mut self, pane: usize) {
        let pane = pane.min(self.screen_pane_count().saturating_sub(1));
        self.screen_focus = pane;
        match self.route().clone() {
            Route::Ide { .. } => {
                self.ide_ui.focus = if pane == 0 {
                    crate::screens::ide::IdeFocus::Tree
                } else {
                    crate::screens::ide::IdeFocus::Editor
                };
            }
            Route::Workflows { .. } => {
                self.workflows_ui.focus = if pane == 0 {
                    crate::screens::workflows::WorkflowFocus::Palette
                } else {
                    crate::screens::workflows::WorkflowFocus::Steps
                };
            }
            Route::TaskGit { tab, .. } => {
                if !matches!(tab, TaskGitTab::Files) {
                    self.task_git_ui.focus = if pane == 0 {
                        crate::screens::task_git::TaskGitFocus::Tree
                    } else {
                        crate::screens::task_git::TaskGitFocus::Diff
                    };
                }
            }
            Route::Github { .. } => {
                self.github_ui.focus = if pane == 0 {
                    crate::screens::github::GithubFocus::List
                } else {
                    crate::screens::github::GithubFocus::Detail
                };
            }
            _ => {}
        }
    }

    fn window_navigation_blocked(&self) -> bool {
        matches!(
            self.route(),
            Route::Github { .. } if self.github_ui.focus == crate::screens::github::GithubFocus::SkillPicker
        ) || matches!(
            self.route(),
            Route::Settings { .. } | Route::GlobalSettings
                if self.settings_ui.file_editing || self.settings_ui.edit.is_some()
        ) || matches!(
            self.route(),
            Route::Workflows { .. }
                if self.workflows_ui.import_open
                    || self.workflows_ui.name_open
                    || self.workflows_ui.delete_confirm.is_some()
        ) || matches!(
            self.route(),
            Route::RepoGit { .. } if self.repo_git_ui.new_branch_open
        ) || matches!(
            self.route(),
            Route::TaskGit { .. } if self.task_git_ui.commit_dialog_open
        )
    }

    fn focus_location(&self) -> FocusLocation {
        if self.sidebar_focus {
            FocusLocation::Sidebar
        } else {
            FocusLocation::Screen(self.current_screen_pane())
        }
    }

    fn set_focus_location(&mut self, location: FocusLocation) {
        let current = self.focus_location();
        if current == location {
            return;
        }
        self.previous_focus = Some(current);
        match location {
            FocusLocation::Sidebar => self.focus_sidebar(),
            FocusLocation::Screen(pane) => {
                self.sidebar_focus = false;
                self.set_screen_pane(pane);
            }
        }
    }

    /// Apply Neovim's spatial window grammar. The sidebar is the leftmost window and each
    /// screen pane follows it in visual order. There are currently no vertically stacked panes,
    /// so `Ctrl-W j/k` deliberately leave focus unchanged.
    fn focus_window(&mut self, direction: VimDirection) {
        if self.window_navigation_blocked() {
            return;
        }
        let count = self.screen_pane_count();
        let target = match (direction, self.focus_location()) {
            (VimDirection::Right, FocusLocation::Sidebar) if count > 0 => {
                Some(FocusLocation::Screen(0))
            }
            (VimDirection::Left, FocusLocation::Screen(0)) => Some(FocusLocation::Sidebar),
            (VimDirection::Left, FocusLocation::Screen(pane)) if pane > 0 => {
                Some(FocusLocation::Screen(pane - 1))
            }
            (VimDirection::Right, FocusLocation::Screen(pane)) if pane + 1 < count => {
                Some(FocusLocation::Screen(pane + 1))
            }
            _ => None,
        };
        if let Some(target) = target {
            if target == FocusLocation::Sidebar && matches!(self.route(), Route::NewTask { .. }) {
                self.new_task_ui.composer_focused = false;
                self.new_task_ui.composer.blur();
            }
            self.set_focus_location(target);
        }
    }

    fn cycle_window(&mut self) {
        if self.window_navigation_blocked() {
            return;
        }
        let count = self.screen_pane_count();
        let target = match self.focus_location() {
            FocusLocation::Sidebar if count > 0 => FocusLocation::Screen(0),
            FocusLocation::Screen(pane) if pane + 1 < count => FocusLocation::Screen(pane + 1),
            _ => FocusLocation::Sidebar,
        };
        self.set_focus_location(target);
    }

    fn restore_previous_window(&mut self) {
        if self.window_navigation_blocked() {
            return;
        }
        if let Some(previous) = self.previous_focus {
            let current = self.focus_location();
            match previous {
                FocusLocation::Sidebar => self.focus_sidebar(),
                FocusLocation::Screen(pane) => {
                    self.sidebar_focus = false;
                    self.set_screen_pane(pane);
                }
            }
            self.previous_focus = Some(current);
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) -> bool {
        let rows = self.sidebar_rows();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.sidebar_selected = (self.sidebar_selected + 1) % rows.len();
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sidebar_selected = (self.sidebar_selected + rows.len() - 1) % rows.len();
                true
            }
            KeyCode::Enter => {
                let Some(row) = rows.get(self.sidebar_selected).copied() else {
                    return false;
                };
                match row {
                    SidebarRow::Project(index) => {
                        if let Some(project) = self.projects.get(index) {
                            self.select_project(project.id.clone());
                            self.sidebar_focus = true;
                        }
                    }
                    SidebarRow::Nav(nav) => {
                        self.sidebar_focus = false;
                        self.navigate(nav);
                    }
                    SidebarRow::GlobalTasks => {
                        self.sidebar_focus = false;
                        self.request_navigate(Route::GlobalTasks);
                        self.queue_pending(PendingAction::RefreshIndex);
                    }
                    SidebarRow::GlobalSettings => {
                        self.sidebar_focus = false;
                        crate::screens::settings::open_global(self);
                    }
                    SidebarRow::Filter(filter) => self.set_task_filter(filter),
                }
                true
            }
            KeyCode::Esc | KeyCode::Right => {
                self.sidebar_focus = false;
                true
            }
            _ => false,
        }
    }

    fn request_quit(&mut self) {
        if self.ide_ui.dirty {
            self.confirm = Some(ConfirmRequest {
                text: "Unsaved changes in the IDE will be lost. Quit anyway?".to_owned(),
                action: PendingAction::Quit,
            });
        } else if self.running_count() > 0 {
            self.confirm = Some(ConfirmRequest {
                text: "Live tasks are still running. Quit anyway?".to_owned(),
                action: PendingAction::Quit,
            });
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

    fn focus_summary(&self) -> (&'static str, &'static str) {
        if self.sidebar_focus {
            return ("SIDEBAR", "↑↓ choose project or view · Enter open");
        }
        match self.route() {
            Route::Tasks { .. } | Route::GlobalTasks => {
                ("TASKS", "j/k choose task · Enter open · :new")
            }
            Route::NewTask { .. } if self.new_task_ui.composer_focused => {
                ("COMPOSER", "type prompt · Esc normal · Ctrl-W h sidebar")
            }
            Route::NewTask { .. } => ("NEW TASK", "i edit prompt · Ctrl-W h sidebar"),
            Route::Scratchpad { .. } => (
                "SCRATCHPAD",
                "type notes · Shift+arrows select · Ctrl+K clear",
            ),
            Route::Ide { .. } if self.current_screen_pane() == 0 => {
                ("FILE TREE", "↑↓ choose file · Enter open")
            }
            Route::Ide { .. } => ("EDITOR", "edit file · Ctrl+S save"),
            Route::RepoGit { tab, .. } if self.current_screen_pane() == 0 => match tab {
                RepoGitTab::Commits => ("COMMIT LIST", "↑↓ browse · Enter load diff"),
                RepoGitTab::Changes => ("FILE LIST", "↑↓ browse changed files"),
                RepoGitTab::Branches => ("BRANCH LIST", "↑↓ browse branches"),
            },
            Route::RepoGit { .. } => ("GIT DETAIL", "j/k scroll · Ctrl-W h list · gt tabs"),
            Route::TaskGit { .. } if self.current_screen_pane() == 0 => {
                ("TASK FILES", "↑↓ browse · Enter open")
            }
            Route::TaskGit { .. } => ("TASK DIFF", "j/k scroll · Ctrl-W h files · gt tabs"),
            Route::Workflows { .. } if self.current_screen_pane() == 0 => {
                ("SKILL PALETTE", "↑↓ choose skill · Enter add")
            }
            Route::Workflows { .. } => ("WORKFLOW STEPS", "↑↓ choose step"),
            Route::Github { .. } if self.current_screen_pane() == 0 => {
                ("GITHUB LIST", "↑↓ choose item · Enter open")
            }
            Route::Github { .. } => ("GITHUB DETAIL", "j/k scroll · Ctrl-W h list · gt tabs"),
            Route::Settings { .. } | Route::GlobalSettings => {
                if self.current_screen_pane() == 0 {
                    ("SETTINGS NAV", "j/k choose section · Ctrl-W l values")
                } else {
                    ("SETTINGS VALUES", "↑↓ choose setting · ←/→ change")
                }
            }
            _ => ("CONTENT", "Ctrl-W h/l changes window"),
        }
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
        NavItem::Scratchpad => HitAction::Scratchpad,
        NavItem::Ide => HitAction::Ide,
        NavItem::Terminal => HitAction::Terminal,
        NavItem::RepoGit => HitAction::RepoGit,
        NavItem::Github => HitAction::Github,
        NavItem::Skills => HitAction::Skills,
        NavItem::Workflows => HitAction::Workflows,
        NavItem::Settings => HitAction::Settings,
    }
}

/// Keyboard handling for the open row menu. Returns true when consumed.
pub fn handle_row_menu_key(app: &mut App, menu: &RowMenu, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(open) = app.row_menu.as_mut() {
                open.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1));
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(open) = app.row_menu.as_mut() {
                open.selected = menu.selected.saturating_sub(1);
            }
            true
        }
        KeyCode::Enter => {
            let selected = menu.selected;
            let Some(action) = menu.items.get(selected).map(|item| item.action) else {
                return true;
            };
            app.row_menu = None;
            apply_menu_action(app, action);
            true
        }
        KeyCode::Esc => {
            app.row_menu = None;
            true
        }
        _ => false,
    }
}

fn apply_menu_action(app: &mut App, action: MenuAction) {
    let Some(menu) = app.row_menu.take() else {
        return;
    };
    let project = menu.project;
    let id = menu.run_id;
    match action {
        MenuAction::Open => crate::screens::thread::open(app, &project, &id),
        MenuAction::Archive => app.pending.push(PendingAction::Archive {
            project,
            id,
            archived: true,
        }),
        MenuAction::Restore => app.pending.push(PendingAction::Archive {
            project,
            id,
            archived: false,
        }),
        MenuAction::MarkRead => app.pending.push(PendingAction::Read { project, id }),
        MenuAction::MarkUnread => app.pending.push(PendingAction::Unread { project, id }),
        MenuAction::Delete => {
            let title = menu.title;
            app.confirm = Some(ConfirmRequest {
                text: format!("Delete \"{title}\" and its branch?"),
                action: PendingAction::Delete { project, id },
            });
        }
        MenuAction::OpenPr => {
            let url = run_reference_url(app, &project, &id);
            if let Some(url) = url {
                app.open_url(&url);
            } else {
                app.notice = Some("no PR or issue URL on this task".to_owned());
            }
        }
        MenuAction::CopyBranch => {
            let branch = run_branch(app, &project, &id);
            if let Some(branch) = branch {
                app.copy_text(&branch);
            } else {
                app.notice = Some("this task has no branch".to_owned());
            }
        }
    }
}

fn run_reference_url(app: &App, project: &str, id: &str) -> Option<String> {
    if project == app.current_project()
        && let Some(run) = app.tasks.iter().find(|run| run.record.id == id)
    {
        return run
            .record
            .pull_request_url
            .clone()
            .or_else(|| run.record.referenced_pull_request_url.clone())
            .or_else(|| run.record.referenced_issue_url.clone());
    }
    if let Some(entry) = app.global_index.as_ref().and_then(|index| {
        index
            .runs
            .iter()
            .find(|entry| entry.project_id == project && entry.id == id)
    }) {
        return entry
            .pull_request_url
            .clone()
            .or_else(|| entry.referenced_pull_request_url.clone())
            .or_else(|| entry.referenced_issue_url.clone());
    }
    None
}

fn run_branch(app: &App, project: &str, id: &str) -> Option<String> {
    if project == app.current_project()
        && let Some(run) = app.tasks.iter().find(|run| run.record.id == id)
    {
        return run.record.branch.clone();
    }
    if let Some(entry) = app.global_index.as_ref().and_then(|index| {
        index
            .runs
            .iter()
            .find(|entry| entry.project_id == project && entry.id == id)
    }) {
        return entry.branch.clone();
    }
    None
}

fn copy_to_clipboard(text: &str) -> bool {
    for candidate in [
        &["wl-copy", text][..],
        &["xclip", "-selection", "clipboard", text][..],
        &["xsel", "--clipboard", "--input", text][..],
        &["pbcopy", text][..],
    ] {
        let (command, args) = candidate.split_first().unwrap_or((&candidate[0], &[]));
        let process = std::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut process) = process {
            use std::io::Write;
            if let Some(stdin) = process.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = process.wait();
            return true;
        }
    }
    false
}

fn sidebar_line(value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(Span::styled(value.into(), style))
}

fn sidebar_nav_line(
    label: &str,
    badge: Option<usize>,
    active: bool,
    focused: bool,
    style: Style,
) -> Line<'static> {
    let style = if focused {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    };
    let mut spans = vec![Span::styled(
        if focused {
            "  ▸ "
        } else if active {
            "  > "
        } else {
            "    "
        },
        style,
    )];
    spans.push(Span::styled(label.to_owned(), style));
    if let Some(badge) = badge {
        spans.push(Span::styled(format!("  [{badge}]"), style));
    }
    Line::from(spans)
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

fn local_prompt_preview(task: &str) -> Option<String> {
    let collapsed = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut chars = collapsed.chars();
    let mut preview: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    Some(preview)
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

trait UppercaseTitle {
    fn uppercase_title(self) -> &'static str;
}

impl UppercaseTitle for NavItem {
    fn uppercase_title(self) -> &'static str {
        match self {
            Self::NewTask => "NEW TASK",
            Self::Tasks => "TASKS",
            Self::Scratchpad => "SCRATCHPAD",
            Self::Ide => "IDE",
            Self::Terminal => "TERMINAL",
            Self::RepoGit => "GIT",
            Self::Github => "GITHUB",
            Self::Skills => "SKILLS",
            Self::Workflows => "WORKFLOWS",
            Self::Settings => "SETTINGS",
        }
    }
}

#[cfg(test)]
mod tests {
    use coducktor_contract::{RunIndexEntry, RunRecord};
    use crossterm::event::{Event, KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn ctrl_w(app: &mut App, suffix: char) {
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char(suffix),
            KeyModifiers::NONE,
        )));
    }

    fn tab_command(app: &mut App, suffix: char) {
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
        )));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char(suffix),
            KeyModifiers::NONE,
        )));
    }

    #[test]
    fn debug_hud_renders_sanitized_frame_metrics_in_the_status_bar() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_debug_hud(true);
        app.record_frame_metrics(12_345, 4_567, 256);
        app.record_dropped_events(3);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let buffer = terminal.backend().buffer();
        let status: String = buffer.content[(23 * 120)..]
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(status.contains("FRAME 12.3ms"), "got: {status}");
        assert!(status.contains("PROJ 4.6ms"), "got: {status}");
        assert!(status.contains("REDUCED 256"), "got: {status}");
        assert!(status.contains("DROPPED 3"), "got: {status}");
    }

    #[test]
    fn taking_a_bounded_pending_batch_preserves_the_fifo_tail() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.pending.push(PendingAction::RefreshTasks {
            project: "first".to_owned(),
        });
        app.pending.push(PendingAction::RefreshTasks {
            project: "second".to_owned(),
        });
        app.pending.push(PendingAction::RefreshIndex);

        assert_eq!(
            app.take_pending_up_to(2),
            vec![
                PendingAction::RefreshTasks {
                    project: "first".to_owned(),
                },
                PendingAction::RefreshTasks {
                    project: "second".to_owned(),
                },
            ]
        );
        assert_eq!(app.take_pending(), vec![PendingAction::RefreshIndex]);
    }

    #[test]
    fn duplicate_refreshes_coalesce_but_mutations_remain_ordered() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.queue_pending(PendingAction::RefreshTasks {
            project: "main".to_owned(),
        });
        app.queue_pending(PendingAction::RefreshProjectRegistry);
        app.queue_pending(PendingAction::RefreshProjectRegistry);
        app.queue_pending(PendingAction::RefreshTasks {
            project: "main".to_owned(),
        });
        app.queue_pending(PendingAction::Archive {
            project: "main".to_owned(),
            id: "one".to_owned(),
            archived: true,
        });
        app.queue_pending(PendingAction::Archive {
            project: "main".to_owned(),
            id: "one".to_owned(),
            archived: true,
        });

        assert_eq!(
            app.take_pending(),
            vec![
                PendingAction::RefreshTasks {
                    project: "main".to_owned(),
                },
                PendingAction::RefreshProjectRegistry,
                PendingAction::Archive {
                    project: "main".to_owned(),
                    id: "one".to_owned(),
                    archived: true,
                },
                PendingAction::Archive {
                    project: "main".to_owned(),
                    id: "one".to_owned(),
                    archived: true,
                },
            ]
        );
    }

    #[test]
    fn coalescable_dispatch_tracking_blocks_a_duplicate_and_clears_on_finish() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let action = PendingAction::RefreshTasks {
            project: "main".to_owned(),
        };
        assert!(!app.coalescable_in_flight(&action));

        app.begin_coalescable_dispatch(action.clone());
        assert!(app.coalescable_in_flight(&action));
        // A different project is a different key — never blocked by an unrelated dispatch.
        assert!(!app.coalescable_in_flight(&PendingAction::RefreshTasks {
            project: "other".to_owned(),
        }));

        app.finish_coalescable_dispatch(&action);
        assert!(!app.coalescable_in_flight(&action));
    }

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
        assert_eq!(
            Route::parse("/settings", "main"),
            Some(Route::GlobalSettings)
        );
        assert_eq!(Route::GlobalSettings.path(), "/settings");
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
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(app.route(), Route::Tasks { .. }));
    }

    #[test]
    fn key_navigation_and_history_shortcuts_change_routes() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.execute_command("open /tasks");
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
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Ship shell",
            RunStatus::Running,
            None,
        ));
        assert_eq!(app.running_count(), 1);

        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Ship shell",
            RunStatus::Review,
            None,
        ));
        assert_eq!(app.running_count(), 0);
        assert_eq!(app.needs_you_count(), 1);

        app.apply_workspace_event(WorkspaceEvent::RunDeleted {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });
        assert_eq!(app.needs_you_count(), 0);
    }

    #[test]
    fn idle_turn_end_needs_no_attention_but_a_structured_ask_does() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Answer plainly",
            RunStatus::Running,
            None,
        ));
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Answer plainly",
            RunStatus::Idle,
            None,
        ));
        assert_eq!(app.needs_you_count(), 0);
        assert!(app.take_pending_notifications().is_empty());

        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Answer plainly",
            RunStatus::Waiting,
            None,
        ));
        assert_eq!(app.needs_you_count(), 1);
        assert_eq!(
            app.take_pending_notifications(),
            vec![("Needs your answer".to_owned(), "Answer plainly".to_owned())]
        );
    }

    fn run_event(
        project: &str,
        id: &str,
        title: &str,
        status: RunStatus,
        seen_at: Option<&str>,
    ) -> WorkspaceEvent {
        WorkspaceEvent::Run {
            project: project.to_owned(),
            run: ApiRun {
                record: RunRecord {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    workflow: "quick-task".to_owned(),
                    task: String::new(),
                    status,
                    created_at: "2026-08-15T00:00:00Z".to_owned(),
                    tokens_used: 0.0,
                    archived: false,
                    seen_at: seen_at.map(ToOwned::to_owned),
                    steps: Vec::new(),
                    ..RunRecord::default()
                },
                usage: None,
            },
        }
    }

    fn run_from_event(event: WorkspaceEvent) -> ApiRun {
        let WorkspaceEvent::Run { run, .. } = event else {
            panic!("expected a run event");
        };
        run
    }

    #[test]
    fn stale_project_refreshes_are_rejected_per_generation() {
        let mut app = App::new("a", Theme::detect(), Keymap::default());
        let first_a = app.begin_task_request("a");
        let _ = app.begin_task_request("b");
        let second_a = app.begin_task_request("a");

        assert!(!app.apply_task_response(
            "a",
            first_a,
            Ok(vec![run_from_event(run_event(
                "a",
                "old-a",
                "old",
                RunStatus::Done,
                None,
            ))]),
        ));
        assert!(app.apply_task_response(
            "a",
            second_a,
            Ok(vec![run_from_event(run_event(
                "a",
                "new-a",
                "new",
                RunStatus::Done,
                None,
            ))]),
        ));
        assert_eq!(
            app.task_state("a").unwrap().runs.first().unwrap().record.id,
            "new-a"
        );
    }

    #[test]
    fn deletion_is_qualified_when_projects_reuse_a_run_id() {
        let mut app = App::new("a", Theme::detect(), Keymap::default());
        app.set_tasks_for_project(
            "a".to_owned(),
            vec![run_from_event(run_event(
                "a",
                "same-id",
                "A",
                RunStatus::Done,
                None,
            ))],
        );
        app.set_tasks_for_project(
            "b".to_owned(),
            vec![run_from_event(run_event(
                "b",
                "same-id",
                "B",
                RunStatus::Done,
                None,
            ))],
        );
        app.set_global_index(RunsIndexResponse {
            runs: ["a", "b"]
                .into_iter()
                .map(|project_id| RunIndexEntry {
                    project_id: project_id.to_owned(),
                    id: "same-id".to_owned(),
                    status: RunStatus::Done,
                    created_at: "2026-08-17T00:00:00Z".to_owned(),
                    ..RunIndexEntry::default()
                })
                .collect(),
            per_project_limit: 200,
            truncated: Vec::new(),
        });

        app.select_task("a", "same-id");
        app.apply_workspace_event(WorkspaceEvent::RunDeleted {
            project: "b".to_owned(),
            id: "same-id".to_owned(),
        });

        assert!(
            app.task_state("a")
                .unwrap()
                .runs
                .iter()
                .any(|run| run.record.id == "same-id")
        );
        assert!(app.task_state("b").unwrap().runs.is_empty());
        assert_eq!(
            app.task_state("a").unwrap().selection,
            Some(TaskKey::new("a", "same-id"))
        );
        assert_eq!(app.global_index.as_ref().unwrap().runs.len(), 1);
        assert_eq!(app.global_index.as_ref().unwrap().runs[0].project_id, "a");
    }

    #[test]
    fn project_and_global_filters_and_selection_survive_route_switching() {
        let mut app = App::new("a", Theme::detect(), Keymap::default());
        app.set_tasks_for_project(
            "a".to_owned(),
            vec![run_from_event(run_event(
                "a",
                "a-run",
                "A",
                RunStatus::Done,
                None,
            ))],
        );
        app.set_tasks_for_project(
            "b".to_owned(),
            vec![run_from_event(run_event(
                "b",
                "b-run",
                "B",
                RunStatus::Done,
                None,
            ))],
        );
        app.select_task("a", "a-run");
        app.set_task_filter(TaskFilter::Archived);
        app.navigate_route(Route::GlobalTasks);
        app.set_task_filter(TaskFilter::Archived);
        app.navigate_route(Route::Tasks {
            project: "b".to_owned(),
        });
        app.set_task_filter(TaskFilter::Active);
        app.navigate_route(Route::Tasks {
            project: "a".to_owned(),
        });

        assert_eq!(app.task_view(), TaskView::Archived);
        assert_eq!(app.global_filter, TaskFilter::Archived);
        assert_eq!(
            app.task_state("a").unwrap().selection,
            Some(TaskKey::new("a", "a-run"))
        );
        assert_eq!(app.task_state("b").unwrap().filter, TaskFilter::Active);
    }

    #[test]
    fn sidebar_collapses_at_the_narrow_breakpoint_and_can_open_as_a_drawer() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        assert_eq!(app.sidebar_width(), SIDEBAR_DEFAULT_WIDTH);
        assert!(!app.sidebar_is_visible(80));
        app.apply_hit_action(HitAction::ToggleSidebar);
        assert!(app.sidebar_is_visible(80));
        app.apply_hit_action(HitAction::ToggleSidebar);
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
    fn sidebar_can_be_focused_and_navigated_without_a_mouse() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL,
        )));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert!(matches!(app.route(), Route::Ide { project } if project == "main"));
    }

    #[test]
    fn task_screen_motions_select_rows_after_ctrl_w_l_and_enter_opens_one() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "First task",
            RunStatus::Running,
            None,
        ));
        app.apply_workspace_event(run_event(
            "main",
            "run-2",
            "Second task",
            RunStatus::Running,
            None,
        ));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        assert!(app.sidebar_focus);
        assert_eq!(app.tasks_ui.table.selected, Some(0));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::CONTROL,
        )));
        assert!(!app.sidebar_focus);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.tasks_ui.table.selected, Some(1));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.tasks_ui.table.selected, Some(1));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert!(matches!(
            app.route(),
            Route::Thread { project, id } if project == "main" && id == "run-2"
        ));
    }

    #[test]
    fn stale_new_task_refreshes_are_rejected_per_project_generation() {
        let mut app = App::new("a", Theme::detect(), Keymap::default());
        let first_a = app.begin_new_task_request("a");
        let _ = app.begin_new_task_request("b");
        let second_a = app.begin_new_task_request("a");

        assert!(!app.accepts_new_task_response("a", first_a));
        assert!(app.accepts_new_task_response("a", second_a));
        assert!(!app.accepts_new_task_response("b", second_a));
    }

    #[test]
    fn opening_the_ide_from_the_sidebar_syncs_its_project() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL,
        )));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert!(matches!(app.route(), Route::Ide { project } if project == "main"));
        assert_eq!(app.ide_ui.project, "main");
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::LoadIdeDirectory { project, path: None } if project == "main"
        )));
    }

    #[test]
    fn workspace_settings_is_reachable_from_sidebar_keyboard_navigation() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL,
        )));
        for _ in 0..11 {
            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        }
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        assert_eq!(app.route(), &Route::GlobalSettings);
    }

    #[test]
    fn open_command_to_the_ide_queues_the_root_listing_for_its_project() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.ide_ui.project = "blarchy".to_owned();
        app.execute_command("open /p/coducktor/ide");

        assert!(matches!(
            app.route(),
            Route::Ide { project } if project == "coducktor"
        ));
        assert_eq!(
            app.ide_ui.project, "coducktor",
            "project switch resets the IDE"
        );
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::LoadIdeDirectory { project, path: None } if project == "coducktor"
        )));
    }

    #[test]
    fn open_command_to_global_settings_queues_settings_data() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());

        app.execute_command("open /settings");

        assert_eq!(app.route(), &Route::GlobalSettings);
        assert!(app.pending.iter().any(
            |action| matches!(action, PendingAction::LoadSettings { project } if project == "main")
        ));
    }

    #[test]
    fn ctrl_w_h_and_l_step_one_ide_window_at_a_time() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        ctrl_w(&mut app, 'h');
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(app.route(), Route::Ide { project } if project == "main"));

        // Starts in the tree: Ctrl-W l steps into the editor, one window only.
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Tree);
        ctrl_w(&mut app, 'l');
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Editor);
        assert!(!app.sidebar_focus);
        // Already at the rightmost section: no-op.
        ctrl_w(&mut app, 'l');
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Editor);

        // Ctrl-W h walks back: editor → tree → sidebar, one window per chord.
        ctrl_w(&mut app, 'h');
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Tree);
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);
        // Leftmost: no-op.
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);

        // And forward again: sidebar → tree → editor.
        ctrl_w(&mut app, 'l');
        assert!(!app.sidebar_focus);
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Tree);
        ctrl_w(&mut app, 'l');
        assert_eq!(app.ide_ui.focus, crate::screens::ide::IdeFocus::Editor);
    }

    #[test]
    fn ctrl_w_h_and_l_move_between_sidebar_and_screen() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        assert!(app.sidebar_focus);
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus, "already leftmost, no-op");
        ctrl_w(&mut app, 'l');
        assert!(!app.sidebar_focus);
        ctrl_w(&mut app, 'l');
        assert!(!app.sidebar_focus, "already rightmost, no-op");
    }

    #[test]
    fn ctrl_w_prefix_is_visible_and_window_previous_restores_focus() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.normal_input.prefix_label(), Some("CTRL-W"));

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let status: String = terminal.backend().buffer().content[(23 * 100)..]
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(status.contains("CTRL-W"));

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.sidebar_focus);
        ctrl_w(&mut app, 'p');
        assert!(app.sidebar_focus);
    }

    #[test]
    fn slash_search_and_n_use_the_shared_normal_grammar() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_tasks(vec![
            run_record(1, RunStatus::Done, None),
            run_record(2, RunStatus::Done, None),
        ]);
        ctrl_w(&mut app, 'l');
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        )));
        for character in "Task".chars() {
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.tasks_ui.query, "Task");
        assert_eq!(app.last_search, "Task");

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.tasks_ui.table.selected, Some(1));
    }

    #[test]
    fn ctrl_w_h_releases_the_composer_and_bare_q_is_inert() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::NewTask);
        assert!(app.new_task_ui.composer_focused);
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);
        assert!(!app.new_task_ui.composer_focused);
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));
        assert!(!app.should_quit());
        app.execute_command("q");
        assert!(app.should_quit());
        assert!(app.new_task_ui.composer.text.is_empty());
    }

    #[test]
    fn confirmation_buttons_are_clickable() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.confirm = Some(ConfirmRequest {
            text: "Quit?".to_owned(),
            action: PendingAction::Quit,
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 39,
            row: 15,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.should_quit());
    }

    #[test]
    fn confirming_cancel_closes_the_dialog_and_shows_immediate_feedback() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.thread_ui.data.project = "main".to_owned();
        app.thread_ui.data.run_id = "run-1".to_owned();
        app.confirm = Some(ConfirmRequest {
            text: "Stop this run?".to_owned(),
            action: PendingAction::CancelRun {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
            },
        });

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        )));

        assert!(app.confirm.is_none());
        assert!(app.thread_ui.cancel_pending);
        assert!(matches!(
            app.pending.as_slice(),
            [PendingAction::CancelRun { project, id }]
                if project == "main" && id == "run-1"
        ));
    }

    #[test]
    fn task_ex_commands_reuse_action_availability_and_confirmations() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_tasks(vec![run_record(1, RunStatus::Waiting, None)]);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        ctrl_w(&mut app, 'l');
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
        )));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        app.execute_command("stop");
        assert!(matches!(
            app.confirm.as_ref().map(|confirm| &confirm.action),
            Some(PendingAction::CancelRun { project, id })
                if project == "main" && id == "run-1"
        ));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.confirm.is_none());

        app.execute_command("delete");
        assert_eq!(
            app.notice.as_deref(),
            Some(":delete is not available for this task")
        );
    }

    #[test]
    fn startup_ctrl_w_l_enters_the_tasks_screen() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        assert!(matches!(app.route(), Route::Tasks { .. }));
        assert!(app.sidebar_focus);

        ctrl_w(&mut app, 'l');

        assert!(!app.sidebar_focus);
        assert!(matches!(app.route(), Route::Tasks { .. }));
    }

    #[test]
    fn ctrl_focus_reaches_workflow_palette_then_steps() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::Workflows);
        app.focus_sidebar();

        ctrl_w(&mut app, 'l');
        assert_eq!(
            app.workflows_ui.focus,
            crate::screens::workflows::WorkflowFocus::Palette
        );
        ctrl_w(&mut app, 'l');
        assert_eq!(
            app.workflows_ui.focus,
            crate::screens::workflows::WorkflowFocus::Steps
        );
        ctrl_w(&mut app, 'h');
        assert_eq!(
            app.workflows_ui.focus,
            crate::screens::workflows::WorkflowFocus::Palette
        );
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);
    }

    #[test]
    fn ctrl_focus_reaches_settings_navigation_then_body() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::Settings);
        app.focus_sidebar();

        ctrl_w(&mut app, 'l');
        assert_eq!(app.screen_focus(), 0);
        ctrl_w(&mut app, 'l');
        assert_eq!(app.screen_focus(), 1);
        ctrl_w(&mut app, 'h');
        assert_eq!(app.screen_focus(), 0);
        ctrl_w(&mut app, 'h');
        assert!(app.sidebar_focus);
    }

    #[test]
    fn sidebar_focus_keeps_bare_q_inert_with_a_live_terminal() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::Terminal);
        app.terminal_ui.sessions.insert(
            "main".to_owned(),
            crate::pty::TerminalSession::spawn(std::path::Path::new("/tmp"), 24, 80).unwrap(),
        );
        app.focus_sidebar();

        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));

        assert!(!app.should_quit());
    }

    #[test]
    fn startup_selector_anchors_on_the_current_project_row() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_projects([
            ("blarchy".to_owned(), "blarchy".to_owned()),
            ("main".to_owned(), "main".to_owned()),
            ("syzygy".to_owned(), "syzygy".to_owned()),
        ]);

        assert_eq!(app.sidebar_selected, 1);
        assert_eq!(app.sidebar_selected_row(), Some(SidebarRow::Project(1)));
    }

    #[test]
    fn sidebar_arrow_cycle_ends_at_workspace_settings() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // 12 rows: current project + 9 navs + All tasks + Settings.
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(app.sidebar_selected, 11);
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(app.route(), Route::GlobalSettings));
    }

    #[test]
    fn navigation_reanchors_the_sidebar_selector_on_the_destination_row() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.sidebar_selected, 2);

        app.request_navigate(Route::RepoGit {
            project: "main".to_owned(),
            tab: RepoGitTab::Changes,
        });
        assert_eq!(app.sidebar_selected, 5);

        app.request_navigate(Route::NewTask {
            project: "main".to_owned(),
        });
        assert_eq!(app.sidebar_selected, 1);

        app.request_back();
        assert_eq!(app.sidebar_selected, 5);

        app.request_back();
        assert_eq!(app.sidebar_selected, 1);
    }

    #[test]
    fn repo_git_navigation_initializes_the_project_before_loading() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::RepoGit);

        assert_eq!(app.repo_git_ui.project, "main");
        assert_eq!(app.repo_git_ui.tab, RepoGitTab::Commits);
        assert!(!app.repo_git_ui.changes_loading);
        assert!(app.pending.iter().any(
            |action| matches!(action, PendingAction::LoadRepoGitCommits { project } if project == "main")
        ));
    }

    #[test]
    fn repo_git_tabs_cycle_with_gt_and_g_shift_t() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.navigate(NavItem::RepoGit);

        tab_command(&mut app, 't');
        assert_eq!(app.repo_git_ui.tab, RepoGitTab::Changes);
        tab_command(&mut app, 't');
        assert_eq!(app.repo_git_ui.tab, RepoGitTab::Branches);
        tab_command(&mut app, 'T');
        assert_eq!(app.repo_git_ui.tab, RepoGitTab::Changes);
    }

    #[test]
    fn route_changes_leave_a_single_highlighted_sidebar_row() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_projects([
            ("blarchy".to_owned(), "blarchy".to_owned()),
            ("main".to_owned(), "main".to_owned()),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        // The selector starts on the current project row (main, index 1).
        assert_eq!(app.sidebar_selected, 1);
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.sidebar_selected, 3);

        app.request_navigate(Route::RepoGit {
            project: "main".to_owned(),
            tab: RepoGitTab::Changes,
        });
        let selected = app.sidebar_selected_row();
        assert_eq!(selected, Some(SidebarRow::Nav(NavItem::RepoGit)));

        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let cell = |x: usize, y: usize| &buffer.content[y * 120 + x];
        // The Git row (sidebar row 8) shows the selector; the Tasks row (row 6) does not.
        assert!(
            cell(2, 8)
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            !cell(2, 6)
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    #[test]
    fn clicking_another_project_switches_the_sidebar_context() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_projects([
            ("blarchy".to_owned(), "blarchy".to_owned()),
            ("main".to_owned(), "main".to_owned()),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }));

        assert_eq!(app.default_project, "blarchy");
        assert_eq!(
            app.route(),
            &Route::Tasks {
                project: "blarchy".to_owned()
            }
        );
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::RefreshNewTask { project } if project == "blarchy"
        )));
    }

    #[test]
    fn clicking_a_sidebar_row_moves_the_arrow_selector_there() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.sidebar_selected, 2);
        assert!(app.sidebar_focus);

        // The keyboard continues from the clicked row.
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(app.sidebar_selected, 3);
    }

    #[test]
    fn renders_at_the_three_snapshot_sizes() {
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

    #[test]
    fn help_overlay_lists_neovim_grammar_and_colon_commands() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.execute_command("help");
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(content.contains("Ctrl-W h/j/k/l"));
        assert!(content.contains("gt/gT tab"));
        for command in CommandId::ALL {
            assert!(
                content.contains(command.usage()),
                "help is missing {}",
                command.usage()
            );
        }
    }

    #[test]
    fn a_started_run_appears_in_the_table_and_progresses_through_statuses() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.now_epoch = 1_800_000_000;
        let render = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let content: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            content
        };

        // A queued run event makes a row appear.
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Ship the shell",
            RunStatus::Queued,
            None,
        ));
        let content = render(&mut app);
        assert!(content.contains("Ship the shell"), "row must appear");
        assert!(content.contains("queued"), "status must read queued");

        // A later event progresses the row in place.
        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Ship the shell",
            RunStatus::Running,
            None,
        ));
        let content = render(&mut app);
        assert!(
            content.contains("running"),
            "status must progress to running"
        );

        app.apply_workspace_event(run_event(
            "main",
            "run-1",
            "Ship the shell",
            RunStatus::Review,
            None,
        ));
        let content = render(&mut app);
        assert!(
            content.contains("needs rev"),
            "status must progress to review, got: {content}"
        );

        // Deletion removes the row.
        app.apply_workspace_event(WorkspaceEvent::RunDeleted {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });
        let content = render(&mut app);
        assert!(!content.contains("Ship the shell"), "row must disappear");
    }

    #[test]
    fn keyboard_moves_the_selection_and_queues_actions() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.set_tasks(vec![
            run_record(1, RunStatus::Done, None),
            run_record(2, RunStatus::Waiting, None),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        ctrl_w(&mut app, 'l');
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
        )));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        app.execute_command("archive");
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::Archive { project, id, archived: true }
                if project == "main" && id == "run-1"
        )));
    }

    fn run_record(index: u8, status: RunStatus, seen_at: Option<&str>) -> ApiRun {
        let mut event = run_event(
            "main",
            &format!("run-{index}"),
            &format!("Task {index}"),
            status,
            seen_at,
        );
        if let WorkspaceEvent::Run { run, .. } = &mut event {
            run.record.title_summary = None;
        }
        match event {
            WorkspaceEvent::Run { run, .. } => run,
            _ => unreachable!(),
        }
    }
}
