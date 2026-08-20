//! The Settings screens. The project route exposes all nine sections; the global route exposes
//! the six workspace sections (Agents, Projects, Appearance, Accounts, Notifications, Resources).
//! Writes go through the in-process engine and durable workspace files.
//!
//! The screen contains only the sections listed above. Terminal-only concerns such as
//! keymaps and external-link safety stay in their owning screens or local configuration. The
//! Appearance controls are persisted in the workspace UI state.
//! Resources renders the sanitized provider usage windows returned by the engine alongside its
//! editable knobs. Per-project account overrides are read-only here; the "Default
//! account" rows write the WORKSPACE default (`projectId: None`) only, not a per-project
//! pin. The Agent config file editor has no dirty-guard confirm (unlike the IDE's) — `Esc`
//! discards a pending edit outright. Prompt templates carry no `skills` auto-apply list.

use coducktor_contract::{
    AgentConfigFileContent, AgentConfigListing, AgentDefaultsPatch, AgentProfilesResponse,
    Appearance, ComposerDefaultsPatch, ConfigResponse, NotificationsUiState,
    ProjectComposerDefaults, PromptTemplate, ReasoningEffort, Runner, RunnerModelCatalogResponse,
    RunnerModelsPatch, SelectAgentProfileInput, SetConfigInput, SetWorkspaceConfigInput,
    TaskSource, UiState, UpdateAgentProfileInput, UpdateProjectInput, WorkspaceConfigResponse,
    WorkspaceUiState, WorkspaceUsageResponse, WorktreesResponse, runner_discovers_models,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, ConfirmRequest, PendingAction, Route};
use crate::diff::Highlighter;
use crate::theme::{Theme, ThemeName};
use crate::widgets::editor::Editor;
use crate::widgets::picker::{Picker, PickerEvent, PickerItem};

const RUNNERS: [Runner; 4] = [Runner::Claude, Runner::Codex, Runner::OpenCode, Runner::Pi];
const THEMES: [ThemeName; 3] = [ThemeName::Dark, ThemeName::LazyVim, ThemeName::Lakes];

pub fn runner_label(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Agents,
    AgentConfig,
    Worktrees,
    PromptTemplates,
    Accounts,
    Appearance,
    Notifications,
    Resources,
    Projects,
}

const SECTIONS: [SettingsSection; 9] = [
    SettingsSection::Agents,
    SettingsSection::AgentConfig,
    SettingsSection::Worktrees,
    SettingsSection::PromptTemplates,
    SettingsSection::Accounts,
    SettingsSection::Appearance,
    SettingsSection::Notifications,
    SettingsSection::Resources,
    SettingsSection::Projects,
];

const GLOBAL_SECTIONS: [SettingsSection; 6] = [
    SettingsSection::Agents,
    SettingsSection::Projects,
    SettingsSection::Appearance,
    SettingsSection::Accounts,
    SettingsSection::Notifications,
    SettingsSection::Resources,
];

impl SettingsSection {
    fn title(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::AgentConfig => "Agent config",
            Self::Worktrees => "Worktrees",
            Self::PromptTemplates => "Prompt templates",
            Self::Accounts => "Agent accounts",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
            Self::Resources => "Resources",
            Self::Projects => "Projects",
        }
    }

    fn scope_label(self) -> &'static str {
        match self {
            Self::Agents | Self::AgentConfig | Self::Worktrees | Self::PromptTemplates => "project",
            _ => "global",
        }
    }
}

/// One text/number field being edited inline (repo_git.rs's new-branch prompt pattern).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsEdit {
    pub buffer: String,
    pub target: EditTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScope {
    Project,
    Global,
}

#[derive(Debug, Clone)]
pub struct SettingsModelPicker {
    pub scope: ModelScope,
    pub runner: Runner,
    pub picker: Picker,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    BaseBranch,
    Model(Runner),
    GlobalModel(Runner),
    TaskSource,
    ComposerVariants,
    ProjectComposerVariants,
    SystemPrompt,
    WorktreeRetention,
    MaxParallel,
    MaxMonitoringSessions,
    MonitoringWakeInterval,
    MemoryLimitMb,
    WorktreeRetentionDefault,
    ChecksoutRoot,
    AccountNewDir(Runner),
    AccountRename(String),
    ProjectRoot,
    ProjectMaxParallel(String),
    /// Prompt-template edit: `index` is `None` for a new entry; `stage` 0 edits the label,
    /// 1 edits the body (the label typed at stage 0 travels in `label`).
    TemplateLabel {
        index: Option<usize>,
    },
    TemplateText {
        index: Option<usize>,
        label: String,
    },
}

pub struct SettingsUi {
    pub project: String,
    pub section: usize,
    pub row: usize,
    pub edit: Option<SettingsEdit>,
    pub notice: Option<String>,
    pub model_picker: Option<SettingsModelPicker>,
    pub model_catalog: Option<RunnerModelCatalogResponse>,

    pub config: Option<ConfigResponse>,
    pub workspace_config: Option<WorkspaceConfigResponse>,
    pub workspace_ui_state: Option<WorkspaceUiState>,
    pub workspace_usage: Option<WorkspaceUsageResponse>,
    pub ui_state: Option<UiState>,
    pub agent_config: Option<AgentConfigListing>,
    pub agent_profiles: Option<AgentProfilesResponse>,
    pub worktrees: Option<WorktreesResponse>,

    pub open_file: Option<AgentConfigFileContent>,
    /// File ID for the in-flight config-file read. It makes a fast selection change a
    /// route-derived request key, so an older completion cannot open the wrong file.
    pub loading_file: Option<String>,
    pub file_editing: bool,
    pub file_editor: Editor,
    pub file_highlighter: Highlighter,
    pub file_viewport: usize,

    /// Provider cycled by ←/→ on the Accounts screen's "+ Add account" row, before Enter
    /// opens the config-dir text prompt.
    pub add_account_provider: usize,
}

impl Default for SettingsUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            section: 0,
            row: 0,
            edit: None,
            notice: None,
            model_picker: None,
            model_catalog: None,
            config: None,
            workspace_config: None,
            workspace_ui_state: None,
            workspace_usage: None,
            ui_state: None,
            agent_config: None,
            agent_profiles: None,
            worktrees: None,
            open_file: None,
            loading_file: None,
            file_editing: false,
            file_editor: Editor::default(),
            file_highlighter: Highlighter::new(),
            file_viewport: 20,
            add_account_provider: 0,
        }
    }
}

pub fn open(app: &mut App, project: &str) {
    if app.settings_ui.project != project {
        app.settings_ui = SettingsUi {
            project: project.to_owned(),
            ..SettingsUi::default()
        };
    }
    app.settings_ui.section = 0;
    app.settings_ui.row = 0;
    app.settings_ui.edit = None;
    app.settings_ui.loading_file = None;
    app.settings_ui.file_editing = false;
    app.request_navigate(Route::Settings {
        project: project.to_owned(),
    });
    app.set_screen_focus(1);
    app.pending.push(PendingAction::LoadSettings {
        project: project.to_owned(),
    });
}

pub fn open_global(app: &mut App) {
    let project = app.current_project().to_owned();
    app.settings_ui = SettingsUi {
        project: project.clone(),
        ..SettingsUi::default()
    };
    app.request_navigate(Route::GlobalSettings);
    app.set_screen_focus(1);
    app.pending.push(PendingAction::LoadSettings { project });
}

fn visible_sections(app: &App) -> &'static [SettingsSection] {
    if matches!(app.route(), Route::GlobalSettings) {
        &GLOBAL_SECTIONS
    } else {
        &SECTIONS
    }
}

fn current_section(app: &App) -> SettingsSection {
    let sections = visible_sections(app);
    sections[app.settings_ui.section.min(sections.len() - 1)]
}

// ---- row model -----------------------------------------------------------------------------

struct Row {
    label: String,
    value: String,
    editable: bool,
}

fn row(label: impl Into<String>, value: impl Into<String>) -> Row {
    Row {
        label: label.into(),
        value: value.into(),
        editable: true,
    }
}

fn opt_str(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "—".to_owned())
}

fn opt_num(value: Option<u64>) -> String {
    value
        .map(|n| n.to_string())
        .unwrap_or_else(|| "—".to_owned())
}

fn bool_label(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn rows_for(app: &App, section: SettingsSection) -> Vec<Row> {
    match section {
        SettingsSection::Agents => rows_agents(app),
        SettingsSection::AgentConfig => rows_agent_config(app),
        SettingsSection::Worktrees => rows_worktrees(app),
        SettingsSection::PromptTemplates => rows_prompt_templates(app),
        SettingsSection::Accounts => rows_accounts(app),
        SettingsSection::Appearance => rows_appearance(app),
        SettingsSection::Notifications => rows_notifications(app),
        SettingsSection::Resources => rows_resources(app),
        SettingsSection::Projects => rows_projects(app),
    }
}

fn rows_agents(app: &App) -> Vec<Row> {
    if matches!(app.route(), Route::GlobalSettings) {
        return rows_global_agents(app);
    }
    let Some(config) = &app.settings_ui.config else {
        return vec![row("Loading…", "")];
    };
    let workspace = app.settings_ui.workspace_config.as_ref();
    let composer = workspace.map(|config| &config.composer_defaults);
    let project_composer = config.composer_defaults.as_ref();
    let source = app
        .settings_ui
        .ui_state
        .as_ref()
        .and_then(|state| state.last_task.as_ref())
        .map(task_source_label)
        .unwrap_or_else(|| "execution".to_owned());
    let reasoning = composer
        .and_then(|defaults| defaults.reasoning)
        .map(reasoning_label)
        .map(str::to_owned)
        .unwrap_or_else(|| "auto".to_owned());
    let variants = composer.and_then(|defaults| defaults.variants).unwrap_or(1);
    let worktree = composer
        .map(|defaults| defaults.worktree.unwrap_or(defaults.inherited_worktree))
        .unwrap_or(false);
    let autonomous = composer
        .and_then(|defaults| {
            defaults.autonomous.or(match defaults.inherited_autonomous {
                coducktor_contract::InheritedAutonomous::Value(value) => Some(value),
                coducktor_contract::InheritedAutonomous::SourceDependent => None,
            })
        })
        .unwrap_or(true);
    let rows = vec![
        row("Base branch", opt_str(&config.base_branch)),
        row(
            "Project runner",
            runner_selection_label(config.default_runner),
        ),
        row(
            "Project model — claude",
            opt_str(&config.default_models.claude),
        ),
        row(
            "Project model — codex",
            opt_str(&config.default_models.codex),
        ),
        row(
            "Project model — opencode",
            opt_str(&config.default_models.opencode),
        ),
        row("Project model — pi", opt_str(&config.default_models.pi)),
        row("System prompt", opt_str(&config.system_prompt)),
        row(
            "Live title updates",
            bool_label(config.live_title_updates.unwrap_or(true)),
        ),
        row(
            "Review gate",
            bool_label(config.review_gate.unwrap_or(true)),
        ),
        row("Default task source", source),
        row(
            "Project reasoning",
            project_composer
                .and_then(|defaults| defaults.reasoning)
                .map(reasoning_label)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("inherit ({reasoning})")),
        ),
        row(
            "Project variants",
            project_composer
                .and_then(|defaults| defaults.variants)
                .map(|variants| variants.to_string())
                .unwrap_or_else(|| format!("inherit ({variants})")),
        ),
        row(
            "Project worktree",
            project_composer
                .and_then(|defaults| defaults.worktree)
                .map(bool_label)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("inherit ({})", bool_label(worktree))),
        ),
        row(
            "Project task mode",
            project_composer
                .and_then(|defaults| defaults.autonomous)
                .map(|autonomous| {
                    if autonomous {
                        "autonomous".to_owned()
                    } else {
                        "manual approval".to_owned()
                    }
                })
                .unwrap_or_else(|| {
                    format!(
                        "inherit ({})",
                        if autonomous {
                            "autonomous"
                        } else {
                            "manual approval"
                        }
                    )
                }),
        ),
    ];
    rows
}

fn rows_global_agents(app: &App) -> Vec<Row> {
    let Some(workspace) = app.settings_ui.workspace_config.as_ref() else {
        return vec![row("Loading…", "")];
    };
    let agent = &workspace.agent_defaults;
    let composer = &workspace.composer_defaults;
    let worktree = composer.worktree.unwrap_or(composer.inherited_worktree);
    let autonomous = composer
        .autonomous
        .or(match composer.inherited_autonomous {
            coducktor_contract::InheritedAutonomous::Value(value) => Some(value),
            coducktor_contract::InheritedAutonomous::SourceDependent => None,
        })
        .unwrap_or(true);
    vec![
        row(
            "Default runner",
            agent.runner.map(runner_selection_label).unwrap_or("—"),
        ),
        row(
            "Default model — claude",
            opt_str(
                &agent
                    .models
                    .as_ref()
                    .and_then(|models| models.claude.clone()),
            ),
        ),
        row(
            "Default model — codex",
            opt_str(
                &agent
                    .models
                    .as_ref()
                    .and_then(|models| models.codex.clone()),
            ),
        ),
        row(
            "Default model — opencode",
            opt_str(
                &agent
                    .models
                    .as_ref()
                    .and_then(|models| models.opencode.clone()),
            ),
        ),
        row(
            "Default model — pi",
            opt_str(&agent.models.as_ref().and_then(|models| models.pi.clone())),
        ),
        row(
            "Default reasoning",
            composer.reasoning.map(reasoning_label).unwrap_or("auto"),
        ),
        row(
            "Default variants",
            composer.variants.unwrap_or(1).to_string(),
        ),
        row("Default worktree", bool_label(worktree)),
        row(
            "Default task mode",
            if autonomous {
                "autonomous"
            } else {
                "manual approval"
            },
        ),
    ]
}

fn runner_selection_label(selection: coducktor_contract::RunnerSelection) -> &'static str {
    match selection {
        coducktor_contract::RunnerSelection::Auto => "auto",
        coducktor_contract::RunnerSelection::Claude => "claude",
        coducktor_contract::RunnerSelection::Codex => "codex",
        coducktor_contract::RunnerSelection::OpenCode => "opencode",
        coducktor_contract::RunnerSelection::Pi => "pi",
    }
}

fn reasoning_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Auto => "auto",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "max",
    }
}

fn task_source_label(source: &TaskSource) -> String {
    match source {
        TaskSource::Baseline => "execution".to_owned(),
        TaskSource::Skill { reference } => format!("skill:{reference}"),
        TaskSource::Workflow { reference } => format!("workflow:{reference}"),
    }
}

fn cycle_runner_selection(
    current: coducktor_contract::RunnerSelection,
    backward: bool,
) -> coducktor_contract::RunnerSelection {
    use coducktor_contract::RunnerSelection::*;
    const ORDER: [coducktor_contract::RunnerSelection; 5] = [Auto, Claude, Codex, OpenCode, Pi];
    let position = ORDER
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let len = ORDER.len();
    let next = if backward {
        (position + len - 1) % len
    } else {
        (position + 1) % len
    };
    ORDER[next]
}

fn rows_agent_config(app: &App) -> Vec<Row> {
    let Some(listing) = &app.settings_ui.agent_config else {
        return vec![row("Loading…", "")];
    };
    if listing.files.is_empty() {
        return vec![row("No agent config files found.", "")];
    }
    listing
        .files
        .iter()
        .map(|file| {
            let mut r = row(
                format!("{}  [{:?}/{:?}]", file.label, file.scope, file.kind),
                if file.exists { "edit" } else { "create" },
            );
            r.editable = file.writable;
            r
        })
        .collect()
}

fn rows_worktrees(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
    let retention = app
        .settings_ui
        .config
        .as_ref()
        .map(|c| c.worktree_retention)
        .unwrap_or(0);
    rows.push(row("Finished worktrees kept", retention.to_string()));
    rows.push(row("Reclaim now", "run"));
    if let Some(worktrees) = &app.settings_ui.worktrees {
        for entry in &worktrees.worktrees {
            let size = entry
                .size_bytes
                .map(|bytes| format!("{:.1} MB", bytes / 1_048_576.0))
                .unwrap_or_else(|| "—".to_owned());
            let mut r = row(
                format!("{}  [{:?}]", entry.title, entry.status),
                format!(
                    "{size}{}",
                    if entry.reclaimable {
                        "  reclaimable"
                    } else {
                        ""
                    }
                ),
            );
            r.editable = false;
            rows.push(r);
        }
    }
    rows
}

fn rows_prompt_templates(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
    let templates = app
        .settings_ui
        .ui_state
        .as_ref()
        .and_then(|state| state.prompt_templates.clone())
        .unwrap_or_default();
    for template in &templates {
        rows.push(row(template.label.clone(), template.text.clone()));
    }
    rows.push(row("+ Add template", ""));
    rows
}

fn rows_accounts(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
    let Some(profiles) = &app.settings_ui.agent_profiles else {
        return vec![row("Loading…", "")];
    };
    for runner in RUNNERS {
        let selected = selected_default_label(profiles, runner);
        rows.push(row(
            format!("Default account — {}", runner_label(runner)),
            selected,
        ));
    }
    for profile in &profiles.profiles {
        let status = profile
            .status
            .as_ref()
            .map(|status| format!("{:?}", status.status))
            .unwrap_or_else(|| "unknown".to_owned());
        rows.push(row(
            format!("{}  [{}]", profile.label, runner_label(profile.provider)),
            format!("{status}  {}", profile.config_dir),
        ));
    }
    rows.push(row(
        format!(
            "+ Add account ({})",
            runner_label(RUNNERS[app.settings_ui.add_account_provider % RUNNERS.len()])
        ),
        "←/→ change provider, Enter to add",
    ));
    rows
}

fn selected_default_label(profiles: &AgentProfilesResponse, runner: Runner) -> String {
    let selection = match runner {
        Runner::Claude => &profiles.defaults.claude,
        Runner::Codex => &profiles.defaults.codex,
        Runner::OpenCode => &profiles.defaults.opencode,
        Runner::Pi => &profiles.defaults.pi,
    };
    match selection {
        Some(id) => profiles
            .profiles
            .iter()
            .find(|profile| &profile.id == id)
            .map(|profile| profile.label.clone())
            .unwrap_or_else(|| id.clone()),
        None => "discovered (default)".to_owned(),
    }
}

fn rows_appearance(app: &App) -> Vec<Row> {
    let appearance = app
        .settings_ui
        .workspace_ui_state
        .as_ref()
        .and_then(|state| state.appearance.clone())
        .unwrap_or_default();
    vec![
        row("Theme", app.theme.name.label().to_owned()),
        row(
            "Density",
            appearance
                .density
                .map(|density| format!("{density:?}").to_lowercase())
                .unwrap_or_else(|| "comfortable".to_owned()),
        ),
        row(
            "Reading width",
            appearance
                .width
                .map(|width| format!("{width:?}").to_lowercase())
                .unwrap_or_else(|| "narrow".to_owned()),
        ),
    ]
}

fn rows_notifications(app: &App) -> Vec<Row> {
    let enabled = app
        .settings_ui
        .workspace_ui_state
        .as_ref()
        .and_then(|state| state.notifications.as_ref())
        .and_then(|notifications| notifications.enabled)
        .unwrap_or(false);
    vec![row("Desktop notifications", bool_label(enabled))]
}

fn rows_resources(app: &App) -> Vec<Row> {
    let Some(config) = &app.settings_ui.workspace_config else {
        return vec![row("Loading…", "")];
    };
    let resources = &config.resources;
    let mut rows = vec![
        row("Max parallel tasks", resources.max_parallel.to_string()),
        row(
            "Max monitoring sessions",
            resources.max_monitoring_sessions.to_string(),
        ),
        row(
            "Monitoring wake interval (min)",
            opt_num(resources.monitoring_wake_interval_minutes),
        ),
        row(
            "Auto-resume on usage limit",
            bool_label(resources.auto_resume_on_usage_limit),
        ),
        row(
            "Intelligent context refresh",
            bool_label(resources.intelligent_context_refresh),
        ),
        row(
            "Memory limit (MB) · unavailable",
            format!(
                "{} · no cross-platform process limiter",
                opt_num(resources.memory_limit_mb)
            ),
        ),
        row(
            "Default worktree retention",
            resources.worktree_retention_default.to_string(),
        ),
    ];
    if let Some(usage) = &app.settings_ui.workspace_usage {
        if let Some(health) = usage.policy_health {
            rows.push(row(
                "Routing health",
                format!(
                    "{}/{} ready · {} unknown",
                    health.ready_candidates, health.total_candidates, health.unknown_candidates
                ),
            ));
        }
        if let Some(refresh) = &usage.refresh
            && let Some(observed_at) = &refresh.observed_at
        {
            rows.push(row("Usage observed", observed_at));
        }
        for provider in &usage.providers {
            let provider_name = match provider.provider {
                coducktor_contract::QuotaProvider::Claude => "Claude",
                coducktor_contract::QuotaProvider::Codex => "Codex",
                coducktor_contract::QuotaProvider::OpenCode => "OpenCode",
            };
            let health = format!("{:?}", provider.health).to_lowercase();
            let upstream = provider
                .upstream_provider
                .as_deref()
                .map(|upstream| format!(" · {upstream}"))
                .unwrap_or_default();
            rows.push(row(
                format!("{provider_name}{upstream} · {}", provider.profile_id),
                format!("{health} · {} · {}", provider.source, provider.fetched_at),
            ));
            for window in &provider.windows {
                let name = match window.id.as_deref() {
                    Some(id) if id.ends_with(":rolling") => "Session window",
                    Some(id) if id.ends_with(":weekly") => "Weekly window",
                    Some(id) if id.ends_with(":monthly") => "Monthly window",
                    _ => match window.kind {
                        coducktor_contract::ProviderUsageWindowKind::Short => "Short window",
                        coducktor_contract::ProviderUsageWindowKind::Long => "Weekly window",
                        coducktor_contract::ProviderUsageWindowKind::Model => "Model window",
                        coducktor_contract::ProviderUsageWindowKind::Unknown => "Usage window",
                    },
                };
                let used = window
                    .used_percent
                    .map(|used| format!("{used:.0}% used"))
                    .unwrap_or_else(|| "usage unknown".to_owned());
                let reset = window
                    .resets_at
                    .as_deref()
                    .map(|reset| format!(" · resets {reset}"))
                    .unwrap_or_default();
                rows.push(row(format!("  {name}"), format!("{used}{reset}")));
            }
            if provider.windows.is_empty()
                && let Some(error) = &provider.error
            {
                rows.push(row("  Limits", &error.message));
            }
        }
    } else {
        rows.push(row("Provider usage", "Loading…"));
    }
    rows
}

fn rows_projects(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.push(row("+ Add repository", "Enter path, then Enter"));
    let root = app
        .settings_ui
        .workspace_config
        .as_ref()
        .map(|c| c.projects_dir.clone())
        .unwrap_or_else(|| "—".to_owned());
    rows.push(row("Checkout root", root));
    for project in &app.project_registry {
        rows.push(row(
            format!("{}  [{:?}]", project.name, project.status),
            format!(
                "{}  max-parallel={}  tags={}",
                project.root,
                project
                    .max_parallel
                    .map(|n| (n as u64).to_string())
                    .unwrap_or_else(|| "inherit".to_owned()),
                project.tags.clone().unwrap_or_default().join(",")
            ),
        ));
    }
    rows
}

// ---- rendering ------------------------------------------------------------------------------

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    if app.settings_ui.file_editing {
        render_file_editor(frame, area, app);
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(1)])
        .split(area);
    render_nav(frame, columns[0], app);
    render_body(frame, columns[1], app);
    if let Some(model_picker) = app.settings_ui.model_picker.as_ref() {
        let theme = app.theme;
        model_picker
            .picker
            .render(frame, area, theme, &mut app.hitmap);
    }
}

fn render_nav(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = if matches!(app.route(), Route::GlobalSettings) {
        "Global settings"
    } else {
        "Settings"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if app.screen_focus() == 0 {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_scope = "";
    for (index, section) in visible_sections(app).iter().enumerate() {
        let scope = if matches!(app.route(), Route::GlobalSettings) {
            "global"
        } else {
            section.scope_label()
        };
        if scope != current_scope {
            current_scope = scope;
            lines.push(Line::from(Span::styled(
                current_scope.to_uppercase(),
                Style::default().fg(app.theme.palette.soft_fg),
            )));
        }
        let mut style = Style::default().fg(app.theme.palette.fg);
        if index == app.settings_ui.section && app.screen_focus() == 0 {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(
            format!(" {}", section.title()),
            style,
        )));
        if let Some(y) = inner.y.checked_add(lines.len() as u16 - 1)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                crate::input::hitmap::HitAction::SettingsSection(index),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_body(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let section = current_section(app);
    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);
    let block =
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(if app.screen_focus() == 1 {
                Style::default().fg(app.theme.palette.accent)
            } else {
                Style::default().fg(app.theme.palette.border)
            });
    let header_inner = block.inner(rows_layout[0]);
    frame.render_widget(block, rows_layout[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            section.title(),
            Style::default()
                .fg(app.theme.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        header_inner,
    );

    let rows = rows_for(app, section);
    let inner = rows_layout[1];
    let mut lines: Vec<Line<'static>> = Vec::new();
    let viewport = inner.height as usize;
    let first_visible = app
        .settings_ui
        .row
        .saturating_sub(viewport.saturating_sub(1));
    for (index, entry) in rows.iter().enumerate().skip(first_visible) {
        let selected = index == app.settings_ui.row && app.screen_focus() == 1;
        let mut label_style = Style::default().fg(app.theme.palette.fg);
        if selected {
            label_style = label_style.add_modifier(Modifier::REVERSED);
        }
        let value = if selected && let Some(edit) = &app.settings_ui.edit {
            format!("{}_", edit.buffer)
        } else {
            entry.value.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<32}", entry.label), label_style),
            Span::styled(
                format!("  {value}"),
                Style::default().fg(app.theme.palette.soft_fg),
            ),
        ]));
        if let Some(y) = inner
            .y
            .checked_add(index.saturating_sub(first_visible) as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                crate::input::hitmap::HitAction::SettingsRow(index),
            );
        }
    }
    if let Some(notice) = &app.settings_ui.notice {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            notice.clone(),
            Style::default().fg(app.theme.palette.accent),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_file_editor(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = app
        .settings_ui
        .open_file
        .as_ref()
        .map(|file| file.path.clone())
        .unwrap_or_else(|| "agent config".to_owned());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title}  (Ctrl+S save, Esc discard)"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.settings_ui.file_viewport = inner.height as usize;
    app.settings_ui
        .file_editor
        .ensure_caret_visible(app.settings_ui.file_viewport);
    let lines = app.settings_ui.file_editor.render_lines(
        &title,
        &app.settings_ui.file_highlighter,
        &app.theme,
        app.settings_ui.file_viewport,
        true,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

// ---- input ----------------------------------------------------------------------------------

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.settings_ui.file_editing {
        return handle_file_editor_key(app, key);
    }
    if let Some(edit) = app.settings_ui.edit.clone() {
        return handle_edit_key(app, edit, key);
    }
    if app.settings_ui.model_picker.is_some() {
        return handle_model_picker_key(app, key);
    }
    match key.code {
        KeyCode::Tab => {
            let sections = visible_sections(app);
            app.settings_ui.section = (app.settings_ui.section + 1) % sections.len();
            app.settings_ui.row = 0;
            true
        }
        KeyCode::BackTab => {
            let sections = visible_sections(app);
            app.settings_ui.section =
                (app.settings_ui.section + sections.len() - 1) % sections.len();
            app.settings_ui.row = 0;
            true
        }
        KeyCode::Char('j') | KeyCode::Down if app.screen_focus() == 0 => {
            let len = visible_sections(app).len();
            app.settings_ui.section = (app.settings_ui.section + 1).min(len.saturating_sub(1));
            app.settings_ui.row = 0;
            true
        }
        KeyCode::Char('k') | KeyCode::Up if app.screen_focus() == 0 => {
            app.settings_ui.section = app.settings_ui.section.saturating_sub(1);
            app.settings_ui.row = 0;
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let len = rows_for(app, current_section(app)).len();
            app.settings_ui.row = (app.settings_ui.row + 1).min(len.saturating_sub(1));
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.settings_ui.row = app.settings_ui.row.saturating_sub(1);
            true
        }
        KeyCode::Left if app.screen_focus() == 1 => {
            cycle(app, true);
            true
        }
        KeyCode::Right if app.screen_focus() == 1 => {
            cycle(app, false);
            true
        }
        KeyCode::Left | KeyCode::Right => true,
        KeyCode::Enter => {
            activate(app);
            true
        }
        KeyCode::Char('d') => {
            delete_row(app);
            true
        }
        KeyCode::Esc => {
            app.request_back();
            true
        }
        _ => false,
    }
}

fn handle_edit_key(app: &mut App, edit: SettingsEdit, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.settings_ui.edit = None;
        }
        KeyCode::Enter => {
            app.settings_ui.edit = None;
            submit_edit(app, edit);
        }
        KeyCode::Backspace => {
            if let Some(edit) = app.settings_ui.edit.as_mut() {
                edit.buffer.pop();
            }
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(edit) = app.settings_ui.edit.as_mut() {
                edit.buffer.push(character);
            }
        }
        _ => {}
    }
    true
}

fn handle_file_editor_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        let Some(open) = app.settings_ui.open_file.clone() else {
            return true;
        };
        app.pending.push(PendingAction::SettingsPutConfigFile {
            project: app.settings_ui.project.clone(),
            id: open.id,
            content: app.settings_ui.file_editor.text.clone(),
            version: open.version,
        });
        return true;
    }
    if key.code == KeyCode::Esc {
        app.settings_ui.file_editing = false;
        return true;
    }
    let editor = &mut app.settings_ui.file_editor;
    match key.code {
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            editor.insert_char(character)
        }
        KeyCode::Enter => editor.insert_newline(),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete_forward(),
        KeyCode::Left => editor.move_left(),
        KeyCode::Right => editor.move_right(),
        KeyCode::Up => editor.move_up(),
        KeyCode::Down => editor.move_down(),
        KeyCode::Home => editor.move_home(),
        KeyCode::End => editor.move_end(),
        _ => {}
    }
    true
}

fn cycle(app: &mut App, backward: bool) {
    let section = current_section(app);
    let row = app.settings_ui.row;
    if section == SettingsSection::Agents && matches!(app.route(), Route::GlobalSettings) {
        cycle_global_agents(app, row, backward);
        return;
    }
    match (section, row) {
        (SettingsSection::Agents, 1) => {
            let Some(config) = app.settings_ui.config.clone() else {
                return;
            };
            let next = cycle_runner_selection(config.default_runner, backward);
            let input = SetConfigInput {
                default_runner: Some(next),
                ..Default::default()
            };
            app.pending.push(PendingAction::SettingsPutConfig {
                project: app.settings_ui.project.clone(),
                input,
            });
        }
        (SettingsSection::Accounts, row) if row == accounts_add_row(app) => {
            let len = RUNNERS.len();
            app.settings_ui.add_account_provider = if backward {
                (app.settings_ui.add_account_provider + len - 1) % len
            } else {
                (app.settings_ui.add_account_provider + 1) % len
            };
        }
        (SettingsSection::Agents, 10) => cycle_project_reasoning(app, backward),
        (SettingsSection::Agents, 12) => cycle_project_worktree(app, backward),
        (SettingsSection::Agents, 13) => cycle_project_autonomous(app, backward),
        (SettingsSection::Appearance, index) => cycle_appearance(app, index, backward),
        (SettingsSection::Notifications, 0) => toggle_notifications(app),
        (SettingsSection::Resources, index) => toggle_or_ignore_resource(app, index),
        _ => {}
    }
}

fn cycle_global_agents(app: &mut App, row: usize, backward: bool) {
    match row {
        0 => cycle_global_runner(app, backward),
        5 => cycle_default_reasoning(app, backward),
        7 => cycle_default_worktree(app),
        8 => cycle_default_autonomous(app),
        _ => {}
    }
}

fn cycle_default_reasoning(app: &mut App, backward: bool) {
    let current = app
        .settings_ui
        .workspace_config
        .as_ref()
        .and_then(|config| config.composer_defaults.reasoning)
        .unwrap_or(ReasoningEffort::Auto);
    let order = [
        ReasoningEffort::Auto,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
    ];
    let position = order
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = if backward {
        order[(position + order.len() - 1) % order.len()]
    } else {
        order[(position + 1) % order.len()]
    };
    put_composer_defaults(
        app,
        ComposerDefaultsPatch {
            reasoning: Some(Some(next)),
            ..Default::default()
        },
    );
}

fn cycle_default_worktree(app: &mut App) {
    let current = app
        .settings_ui
        .workspace_config
        .as_ref()
        .map(|config| {
            config
                .composer_defaults
                .worktree
                .unwrap_or(config.composer_defaults.inherited_worktree)
        })
        .unwrap_or(false);
    put_composer_defaults(
        app,
        ComposerDefaultsPatch {
            worktree: Some(Some(!current)),
            ..Default::default()
        },
    );
}

fn cycle_default_autonomous(app: &mut App) {
    let current = app
        .settings_ui
        .workspace_config
        .as_ref()
        .and_then(|config| {
            config.composer_defaults.autonomous.or(
                match config.composer_defaults.inherited_autonomous {
                    coducktor_contract::InheritedAutonomous::Value(value) => Some(value),
                    coducktor_contract::InheritedAutonomous::SourceDependent => None,
                },
            )
        })
        .unwrap_or(true);
    put_composer_defaults(
        app,
        ComposerDefaultsPatch {
            autonomous: Some(Some(!current)),
            ..Default::default()
        },
    );
}

fn project_composer_defaults(app: &App) -> Option<&ProjectComposerDefaults> {
    app.settings_ui
        .config
        .as_ref()
        .and_then(|config| config.composer_defaults.as_ref())
}

fn put_project_composer_defaults(
    app: &mut App,
    composer_defaults: coducktor_contract::ComposerDefaultsPatch,
) {
    app.pending.push(PendingAction::SettingsPutConfig {
        project: app.settings_ui.project.clone(),
        input: SetConfigInput {
            composer_defaults: Some(composer_defaults),
            ..Default::default()
        },
    });
}

fn cycle_project_reasoning(app: &mut App, backward: bool) {
    let current = project_composer_defaults(app).and_then(|defaults| defaults.reasoning);
    let order = [
        None,
        Some(ReasoningEffort::Auto),
        Some(ReasoningEffort::Low),
        Some(ReasoningEffort::Medium),
        Some(ReasoningEffort::High),
        Some(ReasoningEffort::XHigh),
    ];
    let position = order
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = if backward {
        order[(position + order.len() - 1) % order.len()]
    } else {
        order[(position + 1) % order.len()]
    };
    put_project_composer_defaults(
        app,
        coducktor_contract::ComposerDefaultsPatch {
            reasoning: Some(next),
            ..Default::default()
        },
    );
}

fn cycle_project_worktree(app: &mut App, backward: bool) {
    let current = project_composer_defaults(app).and_then(|defaults| defaults.worktree);
    let order = [None, Some(false), Some(true)];
    let position = order
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = if backward {
        order[(position + order.len() - 1) % order.len()]
    } else {
        order[(position + 1) % order.len()]
    };
    put_project_composer_defaults(
        app,
        coducktor_contract::ComposerDefaultsPatch {
            worktree: Some(next),
            ..Default::default()
        },
    );
}

fn cycle_project_autonomous(app: &mut App, backward: bool) {
    let current = project_composer_defaults(app).and_then(|defaults| defaults.autonomous);
    let order = [None, Some(false), Some(true)];
    let position = order
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = if backward {
        order[(position + order.len() - 1) % order.len()]
    } else {
        order[(position + 1) % order.len()]
    };
    put_project_composer_defaults(
        app,
        coducktor_contract::ComposerDefaultsPatch {
            autonomous: Some(next),
            ..Default::default()
        },
    );
}

fn put_composer_defaults(app: &mut App, composer_defaults: ComposerDefaultsPatch) {
    app.pending.push(PendingAction::SettingsPutWorkspaceConfig {
        input: SetWorkspaceConfigInput {
            composer_defaults: Some(composer_defaults),
            ..Default::default()
        },
    });
}

fn cycle_global_runner(app: &mut App, backward: bool) {
    let current = app
        .settings_ui
        .workspace_config
        .as_ref()
        .and_then(|c| c.agent_defaults.runner)
        .unwrap_or(coducktor_contract::RunnerSelection::Auto);
    app.pending.push(PendingAction::SettingsPutWorkspaceConfig {
        input: SetWorkspaceConfigInput {
            agent_defaults: Some(AgentDefaultsPatch {
                runner: Some(Some(cycle_runner_selection(current, backward))),
                ..Default::default()
            }),
            ..Default::default()
        },
    });
}

fn accounts_add_row(app: &App) -> usize {
    rows_for(app, SettingsSection::Accounts)
        .len()
        .saturating_sub(1)
}

fn cycle_appearance(app: &mut App, row: usize, backward: bool) {
    match row {
        0 => {
            let current = app.theme.name;
            let position = THEMES
                .iter()
                .position(|value| *value == current)
                .unwrap_or(0);
            let len = THEMES.len();
            let next = if backward {
                (position + len - 1) % len
            } else {
                (position + 1) % len
            };
            app.theme = Theme::new(THEMES[next], app.theme.capability);
            let mut appearance = current_appearance(app);
            appearance.theme = Some(match THEMES[next] {
                ThemeName::Dark => coducktor_contract::ThemePreference::Dark,
                ThemeName::LazyVim => coducktor_contract::ThemePreference::Lazyvim,
                ThemeName::Lakes => coducktor_contract::ThemePreference::Lakes,
            });
            put_appearance(app, appearance);
        }
        1 => {
            use coducktor_contract::Density::*;
            let mut appearance = current_appearance(app);
            let next = match appearance.density {
                Some(Comfortable) | None if !backward => Compact,
                Some(Compact) if !backward => Ultra,
                Some(Ultra) if !backward => Comfortable,
                Some(Comfortable) | None => Ultra,
                Some(Compact) => Comfortable,
                Some(Ultra) => Compact,
            };
            appearance.density = Some(next);
            put_appearance(app, appearance);
        }
        2 => {
            let mut appearance = current_appearance(app);
            let next = match appearance.width {
                Some(coducktor_contract::ReadingWidth::Narrow) | None => {
                    coducktor_contract::ReadingWidth::Wide
                }
                Some(coducktor_contract::ReadingWidth::Wide) => {
                    coducktor_contract::ReadingWidth::Narrow
                }
            };
            appearance.width = Some(next);
            put_appearance(app, appearance);
        }
        _ => {}
    }
}

fn current_appearance(app: &App) -> Appearance {
    app.settings_ui
        .workspace_ui_state
        .as_ref()
        .and_then(|state| state.appearance.clone())
        .unwrap_or_default()
}

fn put_appearance(app: &mut App, appearance: Appearance) {
    let input = WorkspaceUiState {
        appearance: Some(appearance),
        ..Default::default()
    };
    app.pending
        .push(PendingAction::SettingsPutWorkspaceUiState { input });
}

fn toggle_notifications(app: &mut App) {
    let current = app
        .settings_ui
        .workspace_ui_state
        .as_ref()
        .and_then(|state| state.notifications.clone())
        .unwrap_or_default();
    let next = NotificationsUiState {
        enabled: Some(!current.enabled.unwrap_or(false)),
        extra: current.extra,
    };
    let input = WorkspaceUiState {
        notifications: Some(next),
        ..Default::default()
    };
    app.pending
        .push(PendingAction::SettingsPutWorkspaceUiState { input });
}

fn toggle_or_ignore_resource(app: &mut App, row: usize) {
    let Some(config) = app.settings_ui.workspace_config.clone() else {
        return;
    };
    let mut patch = coducktor_contract::WorkspaceResourcesPatch::default();
    match row {
        3 => patch.auto_resume_on_usage_limit = Some(!config.resources.auto_resume_on_usage_limit),
        4 => {
            patch.intelligent_context_refresh = Some(!config.resources.intelligent_context_refresh)
        }
        _ => return,
    }
    let input = SetWorkspaceConfigInput {
        resources: Some(patch),
        ..Default::default()
    };
    app.pending
        .push(PendingAction::SettingsPutWorkspaceConfig { input });
}

fn activate(app: &mut App) {
    let section = current_section(app);
    let row = app.settings_ui.row;
    match section {
        SettingsSection::Agents => activate_agents(app, row),
        SettingsSection::AgentConfig => activate_agent_config(app, row),
        SettingsSection::Worktrees => activate_worktrees(app, row),
        SettingsSection::PromptTemplates => activate_prompt_templates(app, row),
        SettingsSection::Accounts => activate_accounts(app, row),
        SettingsSection::Appearance => cycle_appearance(app, row, false),
        SettingsSection::Notifications => toggle_notifications(app),
        SettingsSection::Resources => toggle_or_ignore_resource(app, row),
        SettingsSection::Projects => activate_projects(app, row),
    }
}

fn start_edit(app: &mut App, target: EditTarget, initial: impl Into<String>) {
    app.settings_ui.edit = Some(SettingsEdit {
        buffer: initial.into(),
        target,
    });
}

const CUSTOM_MODEL: &str = "__custom_model__";

fn model_value(app: &App, scope: ModelScope, runner: Runner) -> Option<String> {
    let models = match scope {
        ModelScope::Project => app
            .settings_ui
            .config
            .as_ref()
            .map(|config| &config.default_models),
        ModelScope::Global => app
            .settings_ui
            .workspace_config
            .as_ref()
            .and_then(|config| config.agent_defaults.models.as_ref()),
    }?;
    match runner {
        Runner::Claude => models.claude.clone(),
        Runner::Codex => models.codex.clone(),
        Runner::OpenCode => models.opencode.clone(),
        Runner::Pi => models.pi.clone(),
    }
}

fn model_picker_items(app: &App, scope: ModelScope, runner: Runner) -> Vec<PickerItem> {
    let current = model_value(app, scope, runner);
    let catalog = app
        .settings_ui
        .model_catalog
        .as_ref()
        .filter(|catalog| catalog.runner == runner);
    let mut items = crate::new_task_form::models_for_runner(runner, catalog, &[current.as_deref()])
        .into_iter()
        .map(|model| {
            PickerItem::simple(
                model.id,
                model.label,
                (!model.desc.is_empty()).then_some(model.desc),
            )
        })
        .collect::<Vec<_>>();
    items.push(PickerItem::simple(
        CUSTOM_MODEL,
        "Custom model…",
        Some("Enter a runner-native model id".to_owned()),
    ));
    items
}

fn open_model_picker(app: &mut App, scope: ModelScope, runner: Runner) {
    let mut picker = Picker::new(format!("{} MODEL", runner_label(runner).to_uppercase()));
    picker.searchable = false;
    let items = model_picker_items(app, scope, runner);
    let current = model_value(app, scope, runner).unwrap_or_default();
    picker.set_items(items);
    picker.selected = picker
        .items
        .iter()
        .position(|item| item.value == current)
        .unwrap_or(0);
    app.settings_ui.model_picker = Some(SettingsModelPicker {
        scope,
        runner,
        picker,
    });
    if runner_discovers_models(runner) {
        app.queue_pending(PendingAction::RefreshModels { runner });
    }
}

pub fn apply_model_catalog(app: &mut App, catalog: RunnerModelCatalogResponse) {
    let Some(open) = app.settings_ui.model_picker.as_ref() else {
        return;
    };
    if open.runner != catalog.runner {
        return;
    }
    let scope = open.scope;
    let runner = open.runner;
    app.settings_ui.model_catalog = Some(catalog);
    let items = model_picker_items(app, scope, runner);
    if let Some(open) = app.settings_ui.model_picker.as_mut() {
        open.picker.set_items(items);
    }
}

fn handle_model_picker_key(app: &mut App, key: KeyEvent) -> bool {
    let event = app
        .settings_ui
        .model_picker
        .as_mut()
        .map(|open| open.picker.handle_key(key))
        .unwrap_or(PickerEvent::Close);
    match event {
        PickerEvent::Select(index) => pick_model_index(app, index),
        PickerEvent::Close => app.settings_ui.model_picker = None,
        PickerEvent::Query(_) | PickerEvent::Noop => {}
    }
    true
}

pub fn pick_model_index(app: &mut App, index: usize) {
    let Some(open) = app.settings_ui.model_picker.as_ref() else {
        return;
    };
    let Some(item) = open.picker.items.get(index) else {
        return;
    };
    let scope = open.scope;
    let runner = open.runner;
    let value = item.value.clone();
    app.settings_ui.model_picker = None;
    if value == CUSTOM_MODEL {
        let target = match scope {
            ModelScope::Project => EditTarget::Model(runner),
            ModelScope::Global => EditTarget::GlobalModel(runner),
        };
        start_edit(
            app,
            target,
            model_value(app, scope, runner).unwrap_or_default(),
        );
        return;
    }
    queue_model_update(app, scope, runner, (!value.is_empty()).then_some(value));
}

fn queue_model_update(app: &mut App, scope: ModelScope, runner: Runner, value: Option<String>) {
    let mut models = RunnerModelsPatch::default();
    match runner {
        Runner::Claude => models.claude = Some(value),
        Runner::Codex => models.codex = Some(value),
        Runner::OpenCode => models.opencode = Some(value),
        Runner::Pi => models.pi = Some(value),
    }
    match scope {
        ModelScope::Project => app.pending.push(PendingAction::SettingsPutConfig {
            project: app.settings_ui.project.clone(),
            input: SetConfigInput {
                default_models: Some(models),
                ..Default::default()
            },
        }),
        ModelScope::Global => app.pending.push(PendingAction::SettingsPutWorkspaceConfig {
            input: SetWorkspaceConfigInput {
                agent_defaults: Some(AgentDefaultsPatch {
                    models: Some(models),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }),
    }
}

fn activate_agents(app: &mut App, row: usize) {
    if matches!(app.route(), Route::GlobalSettings) {
        activate_global_agents(app, row);
        return;
    }
    let Some(config) = app.settings_ui.config.clone() else {
        return;
    };
    match row {
        0 => start_edit(
            app,
            EditTarget::BaseBranch,
            config.base_branch.unwrap_or_default(),
        ),
        1 => cycle(app, false),
        2 => open_model_picker(app, ModelScope::Project, Runner::Claude),
        3 => open_model_picker(app, ModelScope::Project, Runner::Codex),
        4 => open_model_picker(app, ModelScope::Project, Runner::OpenCode),
        5 => open_model_picker(app, ModelScope::Project, Runner::Pi),
        6 => start_edit(
            app,
            EditTarget::SystemPrompt,
            config.system_prompt.unwrap_or_default(),
        ),
        7 => {
            let input = SetConfigInput {
                live_title_updates: Some(Some(!config.live_title_updates.unwrap_or(true))),
                ..Default::default()
            };
            app.pending.push(PendingAction::SettingsPutConfig {
                project: app.settings_ui.project.clone(),
                input,
            });
        }
        8 => {
            let input = SetConfigInput {
                review_gate: Some(Some(!config.review_gate.unwrap_or(true))),
                ..Default::default()
            };
            app.pending.push(PendingAction::SettingsPutConfig {
                project: app.settings_ui.project.clone(),
                input,
            });
        }
        9 => start_edit(
            app,
            EditTarget::TaskSource,
            app.settings_ui
                .ui_state
                .as_ref()
                .and_then(|state| state.last_task.as_ref())
                .map(task_source_label)
                .unwrap_or_else(|| "execution".to_owned()),
        ),
        10 => cycle_project_reasoning(app, false),
        11 => start_edit(
            app,
            EditTarget::ProjectComposerVariants,
            app.settings_ui
                .config
                .as_ref()
                .and_then(|config| config.composer_defaults.as_ref())
                .and_then(|defaults| defaults.variants)
                .map(|variants| variants.to_string())
                .unwrap_or_default(),
        ),
        12 | 13 => cycle(app, false),
        _ => {}
    }
}

fn activate_global_agents(app: &mut App, row: usize) {
    match row {
        0 | 5 | 7 | 8 => cycle_global_agents(app, row, false),
        1 => open_model_picker(app, ModelScope::Global, Runner::Claude),
        2 => open_model_picker(app, ModelScope::Global, Runner::Codex),
        3 => open_model_picker(app, ModelScope::Global, Runner::OpenCode),
        4 => open_model_picker(app, ModelScope::Global, Runner::Pi),
        6 => start_edit(
            app,
            EditTarget::ComposerVariants,
            app.settings_ui
                .workspace_config
                .as_ref()
                .and_then(|config| config.composer_defaults.variants)
                .unwrap_or(1)
                .to_string(),
        ),
        _ => {}
    }
}

fn activate_agent_config(app: &mut App, row: usize) {
    let Some(listing) = &app.settings_ui.agent_config else {
        return;
    };
    let Some(file) = listing.files.get(row) else {
        return;
    };
    if !file.writable {
        app.settings_ui.notice = Some(format!(
            "read-only: {}",
            file.read_only_reason.clone().unwrap_or_default()
        ));
        return;
    }
    app.pending.push(PendingAction::SettingsLoadConfigFile {
        project: app.settings_ui.project.clone(),
        id: file.id.clone(),
    });
}

fn activate_worktrees(app: &mut App, row: usize) {
    match row {
        0 => {
            let current = app
                .settings_ui
                .config
                .as_ref()
                .map(|c| c.worktree_retention)
                .unwrap_or(0);
            start_edit(app, EditTarget::WorktreeRetention, current.to_string());
        }
        1 => app.pending.push(PendingAction::SettingsReclaimWorktrees {
            project: app.settings_ui.project.clone(),
        }),
        index => {
            let Some(worktrees) = &app.settings_ui.worktrees else {
                return;
            };
            let Some(entry) = worktrees.worktrees.get(index - 2) else {
                return;
            };
            app.confirm = Some(ConfirmRequest {
                text: format!("Remove the worktree for \"{}\"?", entry.title),
                action: PendingAction::SettingsRemoveWorktree {
                    project: app.settings_ui.project.clone(),
                    run_id: entry.run_id.clone(),
                },
            });
        }
    }
}

fn activate_prompt_templates(app: &mut App, row: usize) {
    let templates = app
        .settings_ui
        .ui_state
        .as_ref()
        .and_then(|state| state.prompt_templates.clone())
        .unwrap_or_default();
    if row == templates.len() {
        start_edit(app, EditTarget::TemplateLabel { index: None }, "");
        return;
    }
    if let Some(template) = templates.get(row) {
        start_edit(
            app,
            EditTarget::TemplateLabel { index: Some(row) },
            template.label.clone(),
        );
    }
}

fn activate_accounts(app: &mut App, row: usize) {
    let Some(profiles) = app.settings_ui.agent_profiles.clone() else {
        return;
    };
    if row < RUNNERS.len() {
        let runner = RUNNERS[row];
        let current = match runner {
            Runner::Claude => &profiles.defaults.claude,
            Runner::Codex => &profiles.defaults.codex,
            Runner::OpenCode => &profiles.defaults.opencode,
            Runner::Pi => &profiles.defaults.pi,
        };
        let candidates: Vec<Option<String>> = std::iter::once(None)
            .chain(
                profiles
                    .profiles
                    .iter()
                    .filter(|profile| profile.provider == runner)
                    .map(|profile| Some(profile.id.clone())),
            )
            .collect();
        let position = candidates
            .iter()
            .position(|candidate| candidate == current)
            .unwrap_or(0);
        let next = candidates[(position + 1) % candidates.len()].clone();
        app.pending.push(PendingAction::SettingsSelectAgentProfile {
            input: SelectAgentProfileInput {
                project_id: None,
                provider: runner,
                profile_id: next,
            },
        });
        return;
    }
    let profile_row = row - RUNNERS.len();
    if profile_row < profiles.profiles.len() {
        let profile = &profiles.profiles[profile_row];
        start_edit(
            app,
            EditTarget::AccountRename(profile.id.clone()),
            profile.label.clone(),
        );
        return;
    }
    let runner = RUNNERS[app.settings_ui.add_account_provider % RUNNERS.len()];
    start_edit(app, EditTarget::AccountNewDir(runner), "");
}

fn activate_projects(app: &mut App, row: usize) {
    if row == 0 {
        start_edit(app, EditTarget::ProjectRoot, "");
        return;
    }
    if row == 1 {
        let current = app
            .settings_ui
            .workspace_config
            .as_ref()
            .map(|c| c.projects_dir.clone())
            .unwrap_or_default();
        start_edit(app, EditTarget::ChecksoutRoot, current);
        return;
    }
    let Some(project) = app.project_registry.get(row - 2) else {
        return;
    };
    start_edit(
        app,
        EditTarget::ProjectMaxParallel(project.id.clone()),
        project
            .max_parallel
            .map(|n| (n as u64).to_string())
            .unwrap_or_default(),
    );
}

fn delete_row(app: &mut App) {
    let section = current_section(app);
    let row = app.settings_ui.row;
    match section {
        SettingsSection::PromptTemplates => {
            let templates = app
                .settings_ui
                .ui_state
                .as_ref()
                .and_then(|state| state.prompt_templates.clone())
                .unwrap_or_default();
            if let Some(template) = templates.get(row) {
                let mut next = templates.clone();
                next.remove(row);
                let mut state = app.settings_ui.ui_state.clone().unwrap_or_default();
                state.prompt_templates = Some(next);
                app.confirm = Some(ConfirmRequest {
                    text: format!("Delete the prompt template \"{}\"?", template.label),
                    action: PendingAction::PutUiState {
                        project: app.settings_ui.project.clone(),
                        state,
                    },
                });
            }
        }
        SettingsSection::Accounts => {
            let Some(profiles) = &app.settings_ui.agent_profiles else {
                return;
            };
            if row >= RUNNERS.len() {
                let profile_row = row - RUNNERS.len();
                if let Some(profile) = profiles.profiles.get(profile_row) {
                    app.confirm = Some(ConfirmRequest {
                        text: format!("Remove the account \"{}\"?", profile.label),
                        action: PendingAction::SettingsRemoveAgentProfile {
                            id: profile.id.clone(),
                        },
                    });
                }
            }
        }
        SettingsSection::Worktrees => activate_worktrees(app, row),
        SettingsSection::Projects if row > 1 => {
            if let Some(project) = app.project_registry.get(row - 2) {
                app.confirm = Some(ConfirmRequest {
                    text: format!("Remove \"{}\" from the project registry?", project.name),
                    action: PendingAction::SettingsRemoveProject {
                        id: project.id.clone(),
                    },
                });
            }
        }
        _ => {}
    }
}

fn submit_edit(app: &mut App, edit: SettingsEdit) {
    let text = edit.buffer.trim().to_owned();
    let project = app.settings_ui.project.clone();
    match edit.target {
        EditTarget::BaseBranch => {
            let input = SetConfigInput {
                base_branch: Some(if text.is_empty() { None } else { Some(text) }),
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutConfig { project, input });
        }
        EditTarget::Model(runner) => {
            let mut models = coducktor_contract::RunnerModelsPatch::default();
            let value = if text.is_empty() { None } else { Some(text) };
            match runner {
                Runner::Claude => models.claude = Some(value),
                Runner::Codex => models.codex = Some(value),
                Runner::OpenCode => models.opencode = Some(value),
                Runner::Pi => models.pi = Some(value),
            }
            let input = SetConfigInput {
                default_models: Some(models),
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutConfig { project, input });
        }
        EditTarget::GlobalModel(runner) => {
            let mut models = RunnerModelsPatch::default();
            let value = if text.is_empty() { None } else { Some(text) };
            match runner {
                Runner::Claude => models.claude = Some(value),
                Runner::Codex => models.codex = Some(value),
                Runner::OpenCode => models.opencode = Some(value),
                Runner::Pi => models.pi = Some(value),
            }
            app.pending.push(PendingAction::SettingsPutWorkspaceConfig {
                input: SetWorkspaceConfigInput {
                    agent_defaults: Some(AgentDefaultsPatch {
                        models: Some(models),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            });
        }
        EditTarget::SystemPrompt => {
            let input = SetConfigInput {
                system_prompt: Some(if text.is_empty() { None } else { Some(text) }),
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutConfig { project, input });
        }
        EditTarget::TaskSource => {
            let Some(source) = parse_task_source(&text) else {
                app.settings_ui.notice =
                    Some("source must be execution, skill:<name>, or workflow:<name>".to_owned());
                return;
            };
            let mut state = app.settings_ui.ui_state.clone().unwrap_or_default();
            state.last_task = Some(source);
            app.settings_ui.ui_state = Some(state.clone());
            app.pending
                .push(PendingAction::PutUiState { project, state });
        }
        EditTarget::ComposerVariants => {
            let Ok(variants) = text.parse::<u64>() else {
                app.settings_ui.notice = Some("default variants must be 1, 2, or 3".to_owned());
                return;
            };
            if !(1..=3).contains(&variants) {
                app.settings_ui.notice = Some("default variants must be 1, 2, or 3".to_owned());
                return;
            }
            put_composer_defaults(
                app,
                ComposerDefaultsPatch {
                    variants: Some(Some(variants)),
                    ..Default::default()
                },
            );
        }
        EditTarget::ProjectComposerVariants => {
            let variants = if text.is_empty() || text.eq_ignore_ascii_case("inherit") {
                None
            } else {
                let Ok(variants) = text.parse::<u64>() else {
                    app.settings_ui.notice =
                        Some("project variants must be 1, 2, 3, or inherit".to_owned());
                    return;
                };
                if !(1..=3).contains(&variants) {
                    app.settings_ui.notice =
                        Some("project variants must be 1, 2, 3, or inherit".to_owned());
                    return;
                }
                Some(variants)
            };
            put_project_composer_defaults(
                app,
                ComposerDefaultsPatch {
                    variants: Some(variants),
                    ..Default::default()
                },
            );
        }
        EditTarget::WorktreeRetention => {
            if let Ok(value) = text.parse::<u64>() {
                let input = SetConfigInput {
                    worktree_retention: Some(Some(value)),
                    ..Default::default()
                };
                app.pending
                    .push(PendingAction::SettingsPutConfig { project, input });
            }
        }
        EditTarget::MaxParallel
        | EditTarget::MaxMonitoringSessions
        | EditTarget::MonitoringWakeInterval
        | EditTarget::MemoryLimitMb
        | EditTarget::WorktreeRetentionDefault => {
            // Reserved for a future numeric-resource picker; Resources' number fields are
            // currently read-only in this cut (toggles and quota routing are the writable
            // knobs — see the module doc's scope-cut list).
        }
        EditTarget::ChecksoutRoot => {
            let input = SetWorkspaceConfigInput {
                projects_dir: if text.is_empty() { None } else { Some(text) },
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutWorkspaceConfig { input });
        }
        EditTarget::AccountNewDir(runner) => {
            if !text.is_empty() {
                app.pending.push(PendingAction::SettingsCreateAgentProfile {
                    provider: runner,
                    config_dir: text,
                });
            }
        }
        EditTarget::AccountRename(id) => {
            if !text.is_empty() {
                app.pending.push(PendingAction::SettingsUpdateAgentProfile {
                    id,
                    input: UpdateAgentProfileInput {
                        label: Some(text),
                        config_dir: None,
                    },
                });
            }
        }
        EditTarget::ProjectRoot => {
            if !text.is_empty() {
                app.pending
                    .push(PendingAction::SettingsRegisterProject { root: text });
            }
        }
        EditTarget::ProjectMaxParallel(id) => {
            let value = if text.is_empty() {
                None
            } else {
                text.parse::<u64>().ok()
            };
            app.pending.push(PendingAction::SettingsUpdateProject {
                id,
                input: UpdateProjectInput {
                    max_parallel: Some(value),
                    tags: None,
                },
            });
        }
        EditTarget::TemplateLabel { index } => {
            if text.is_empty() {
                return;
            }
            start_edit(app, EditTarget::TemplateText { index, label: text }, "");
        }
        EditTarget::TemplateText { index, label } => {
            let mut templates = app
                .settings_ui
                .ui_state
                .as_ref()
                .and_then(|state| state.prompt_templates.clone())
                .unwrap_or_default();
            let entry = PromptTemplate {
                id: index
                    .and_then(|i| templates.get(i).map(|t| t.id.clone()))
                    .unwrap_or_else(|| format!("template-{}", templates.len() + 1)),
                label,
                text,
                skills: None,
            };
            match index {
                Some(i) if i < templates.len() => templates[i] = entry,
                _ => templates.push(entry),
            }
            let mut state = app.settings_ui.ui_state.clone().unwrap_or_default();
            state.prompt_templates = Some(templates);
            app.pending
                .push(PendingAction::PutUiState { project, state });
        }
    }
}

fn parse_task_source(value: &str) -> Option<TaskSource> {
    if value.eq_ignore_ascii_case("execution") || value.eq_ignore_ascii_case("baseline") {
        return Some(TaskSource::Baseline);
    }
    let (prefix, reference) = value.split_once(':')?;
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    match prefix.to_ascii_lowercase().as_str() {
        "skill" => Some(TaskSource::Skill {
            reference: reference.to_owned(),
        }),
        "workflow" => Some(TaskSource::Workflow {
            reference: reference.to_owned(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use coducktor_contract::{
        AgentDefaults, ComposerDefaults, InheritedAutonomous, ProjectComposerDefaults,
        RunnerModels, RunnerSelection, WorkspaceResources,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_config() -> ConfigResponse {
        ConfigResponse {
            base_branch: None,
            default_runner: RunnerSelection::Auto,
            system_prompt: None,
            default_models: RunnerModels::default(),
            composer_defaults: None,
            models_locked: false,
            max_parallel: 2,
            memory_limit_mb: None,
            worktree_retention: 5,
            live_title_updates: Some(true),
            review_gate: Some(true),
        }
    }

    fn sample_workspace_config() -> WorkspaceConfigResponse {
        WorkspaceConfigResponse {
            projects_dir: "/home/user/projects".to_owned(),
            composer_defaults: ComposerDefaults {
                reasoning: None,
                variants: None,
                autonomous: None,
                worktree: None,
                inherited_autonomous: InheritedAutonomous::SourceDependent,
                inherited_worktree: false,
            },
            resources: WorkspaceResources {
                max_parallel: 4,
                max_monitoring_sessions: 2,
                monitoring_wake_interval_minutes: None,
                auto_resume_on_usage_limit: false,
                intelligent_context_refresh: false,
                memory_limit_mb: None,
                worktree_retention_default: 5,
            },
            quota_routing: None,
            agent_defaults: AgentDefaults::default(),
        }
    }

    fn app_with_settings() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.settings_ui.config = Some(sample_config());
        app.settings_ui.workspace_config = Some(sample_workspace_config());
        app
    }

    fn app_with_global_settings() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open_global(&mut app);
        app.settings_ui.workspace_config = Some(sample_workspace_config());
        app.set_project_registry(vec![coducktor_contract::ProjectListEntry {
            id: "main".to_owned(),
            name: "main".to_owned(),
            root: "/home/user/main".to_owned(),
            status: coducktor_contract::ProjectStatus::Ok,
            ..Default::default()
        }]);
        app.take_pending();
        app
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
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
    fn renders_nav_and_the_agents_section_by_default() {
        let mut app = app_with_settings();
        let content = render_text(&mut app, 120, 40);
        assert!(content.contains("Settings"));
        assert!(content.contains("Agents"));
        assert!(content.contains("Base branch"));
        assert!(content.contains("PROJECT"));
        assert!(content.contains("GLOBAL"));
    }

    #[test]
    fn tab_cycles_through_every_section() {
        let mut app = app_with_settings();
        for expected in SECTIONS.iter().skip(1) {
            handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(current_section(&app), *expected);
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(current_section(&app), SettingsSection::Agents);
    }

    #[test]
    fn global_settings_starts_with_agents_and_renders_workspace_sections() {
        let mut app = app_with_global_settings();
        let content = render_text(&mut app, 120, 40);
        assert_eq!(current_section(&app), SettingsSection::Agents);
        assert!(content.contains("Global settings"));
        assert!(content.contains("Default runner"));
        assert!(content.contains("Default model — codex"));
        assert!(content.contains("Default reasoning"));
        assert!(content.contains("Projects"));
        assert!(content.contains("Appearance"));
    }

    #[test]
    fn resources_renders_provider_usage_and_unknown_limits_honestly() {
        let mut app = app_with_global_settings();
        app.settings_ui.section = 5;
        app.settings_ui.workspace_usage = Some(WorkspaceUsageResponse {
            providers: vec![coducktor_contract::ProviderUsageSnapshot {
                provider: coducktor_contract::QuotaProvider::Codex,
                profile_id: "default".to_owned(),
                upstream_provider: None,
                health: coducktor_contract::ProviderUsageHealth::Available,
                confidence: Some(coducktor_contract::UsageConfidence::Authoritative),
                fetched_at: "2026-08-18T00:00:00.000Z".to_owned(),
                source: "codex_app_server".to_owned(),
                stale: false,
                windows: vec![coducktor_contract::ProviderUsageWindow {
                    id: Some("codex:weekly".to_owned()),
                    kind: coducktor_contract::ProviderUsageWindowKind::Long,
                    used_percent: Some(0.0),
                    resets_at: Some("2026-08-25T00:00:00.000Z".to_owned()),
                    hard_limit_reached: Some(false),
                }],
                consumption: None,
                error: None,
                extra: Default::default(),
            }],
            refresh: Some(coducktor_contract::WorkspaceUsageRefresh {
                refreshing: false,
                observed_at: Some("2026-08-18T00:00:00.000Z".to_owned()),
                stale: false,
                error: None,
            }),
            policy_health: Some(coducktor_contract::WorkspaceUsagePolicyHealth {
                ready_candidates: 1,
                total_candidates: 1,
                unknown_candidates: 0,
            }),
        });
        let content = render_text(&mut app, 120, 40);
        assert!(content.contains("Routing health"));
        assert!(content.contains("Codex · default"));
        assert!(content.contains("Weekly window"));
        assert!(content.contains("0% used"));
        assert!(content.contains("Monitoring wake interval (min)"));
        assert!(content.contains("Memory limit (MB) · unavailable"));
        assert!(!content.contains("100% available"));
    }

    #[test]
    fn resources_render_a_status_for_every_retained_policy_control() {
        let mut app = app_with_global_settings();
        app.settings_ui.section = 5;
        let content = render_text(&mut app, 120, 40);

        for (label, unavailable) in [
            ("Max parallel tasks", false),
            ("Max monitoring sessions", false),
            ("Auto-resume on usage limit", false),
            ("Intelligent context refresh", false),
            ("Default worktree retention", false),
            ("Monitoring wake interval (min)", false),
            ("Memory limit (MB) · unavailable", true),
        ] {
            assert!(
                content.contains(label),
                "missing resource policy row: {label}"
            );
            assert_eq!(label.contains("· unavailable"), unavailable);
        }
    }

    #[test]
    fn global_settings_theme_control_persists_the_theme() {
        let mut app = app_with_global_settings();
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(current_section(&app), SettingsSection::Appearance);
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.theme.name, ThemeName::Lakes);
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutWorkspaceUiState { input }
                if input.appearance.as_ref().and_then(|appearance| appearance.theme)
                    == Some(coducktor_contract::ThemePreference::Lakes)
        )));
    }

    #[test]
    fn appearance_menu_uses_theme_without_an_accent_control() {
        let mut app = app_with_global_settings();
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let content = render_text(&mut app, 120, 40);
        assert!(content.contains("Theme"));
        assert!(content.contains("Density"));
        assert!(content.contains("Reading width"));
        assert!(!content.contains("Accent"));
    }

    #[test]
    fn global_projects_add_row_queues_registration() {
        let mut app = app_with_global_settings();
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for character in "/tmp/another-repo".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let pending = app.take_pending();
        assert!(pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsRegisterProject { root } if root == "/tmp/another-repo"
        )));
    }

    #[test]
    fn editing_the_base_branch_field_queues_a_config_write() {
        let mut app = app_with_settings();
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for character in "main".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let pending = app.take_pending();
        assert!(pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. } if input.base_branch == Some(Some("main".to_owned()))
        )));
    }

    #[test]
    fn toggling_review_gate_queues_the_negated_value() {
        let mut app = app_with_settings();
        for _ in 0..8 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let pending = app.take_pending();
        assert!(pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. } if input.review_gate == Some(Some(false))
        )));
    }

    #[test]
    fn project_model_picker_uses_matching_discovered_catalog_and_persists_selection() {
        let mut app = app_with_settings();
        app.settings_ui.row = 3;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.settings_ui.model_picker.as_ref(),
            Some(open) if open.runner == Runner::Codex
        ));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::RefreshModels {
                runner: Runner::Codex
            }
        )));

        apply_model_catalog(
            &mut app,
            RunnerModelCatalogResponse {
                runner: Runner::OpenCode,
                models: vec![coducktor_contract::RunnerModelOption {
                    id: "wrong/model".to_owned(),
                    label: "wrong".to_owned(),
                    description: String::new(),
                    reasoning_efforts: None,
                }],
                source: coducktor_contract::ModelCatalogSource::Live,
                stale: false,
                reason: None,
            },
        );
        assert!(
            !app.settings_ui
                .model_picker
                .as_ref()
                .unwrap()
                .picker
                .items
                .iter()
                .any(|item| item.value == "wrong/model")
        );

        apply_model_catalog(
            &mut app,
            RunnerModelCatalogResponse {
                runner: Runner::Codex,
                models: vec![coducktor_contract::RunnerModelOption {
                    id: "gpt-5.6-codex".to_owned(),
                    label: "GPT-5.6 Codex".to_owned(),
                    description: "Current Codex model".to_owned(),
                    reasoning_efforts: None,
                }],
                source: coducktor_contract::ModelCatalogSource::Live,
                stale: false,
                reason: None,
            },
        );
        let index = app
            .settings_ui
            .model_picker
            .as_ref()
            .unwrap()
            .picker
            .items
            .iter()
            .position(|item| item.value == "gpt-5.6-codex")
            .unwrap();
        app.pending.clear();
        pick_model_index(&mut app, index);
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. }
                if input.default_models.as_ref().and_then(|models| models.codex.as_ref())
                    == Some(&Some("gpt-5.6-codex".to_owned()))
        )));
    }

    #[test]
    fn global_model_picker_writes_workspace_agent_defaults() {
        let mut app = app_with_global_settings();
        app.settings_ui.row = 1;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let opus = app
            .settings_ui
            .model_picker
            .as_ref()
            .unwrap()
            .picker
            .items
            .iter()
            .position(|item| item.value == "opus")
            .unwrap();
        app.pending.clear();
        pick_model_index(&mut app, opus);
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutWorkspaceConfig { input }
                if input.agent_defaults.as_ref()
                    .and_then(|defaults| defaults.models.as_ref())
                    .and_then(|models| models.claude.as_ref())
                    == Some(&Some("opus".to_owned()))
        )));
    }

    #[test]
    fn composer_defaults_are_editable_from_agents_settings() {
        let mut app = app_with_global_settings();
        app.pending.clear();
        app.settings_ui.row = 5;
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutWorkspaceConfig { input }
                if input.composer_defaults.as_ref().and_then(|defaults| defaults.reasoning)
                    == Some(Some(ReasoningEffort::Low))
        )));

        app.pending.clear();
        app.settings_ui.row = 6;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutWorkspaceConfig { input }
                if input.composer_defaults.as_ref().and_then(|defaults| defaults.variants)
                    == Some(Some(3))
        )));
    }

    #[test]
    fn project_composer_defaults_are_editable_and_can_inherit() {
        let mut app = app_with_settings();
        app.pending.clear();
        app.settings_ui.row = 10;
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. }
                if input.composer_defaults.as_ref().and_then(|defaults| defaults.reasoning)
                    == Some(Some(ReasoningEffort::Auto))
        )));

        app.pending.clear();
        app.settings_ui.row = 11;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. }
                if input.composer_defaults.as_ref().and_then(|defaults| defaults.variants)
                    == Some(Some(3))
        )));

        app.settings_ui.config = Some(ConfigResponse {
            composer_defaults: Some(ProjectComposerDefaults {
                reasoning: Some(ReasoningEffort::Auto),
                ..Default::default()
            }),
            ..sample_config()
        });
        app.pending.clear();
        app.settings_ui.row = 10;
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SettingsPutConfig { input, .. }
                if input.composer_defaults.as_ref().and_then(|defaults| defaults.reasoning)
                    == Some(None)
        )));
    }

    #[test]
    fn snapshot_settings_at_three_sizes() {
        let mut app = app_with_settings();
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("settings_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }

    #[test]
    fn snapshot_global_agents_settings() {
        let mut app = app_with_global_settings();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        insta::assert_debug_snapshot!("global_agents_settings", terminal.backend().buffer());
    }
}
