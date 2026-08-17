//! The Settings screen (spec §8.14) — replaces `routes/settings/*`. A registry-driven
//! nav over nine sections; four describe THIS project (Agents, Agent config, Worktrees,
//! Prompt templates), five describe the user/machine (Accounts, Appearance, Notifications,
//! Resources, Projects). Writes go through the same routes the web app uses
//! (`/config`, `/ui-state`, `/workspace/config`, `/workspace/ui-state`,
//! `/workspace/agent-profiles*`, `/agent-config/:id`), so the two clients stay
//! interoperable while both exist (Phase A/B).
//!
//! **Section list, per spec §8.14 verbatim**: the settings screen contains only the nine
//! sections listed above. Terminal-only concerns such as keymaps and external-link safety
//! stay in their owning screens or local configuration rather than becoming settings panels.
//!
//! **Deliberate scope cuts, documented like A8's:** the Theme control changes `app.theme`
//! for the running session only, matching the web app's own comment in
//! `packages/contract/src/workspace.ts` that theme "stays in localStorage — it is
//! per-browser by design" — there is no shared server field for it to round-trip through.
//! Provider usage graphs (`GET /workspace/usage`) are not rendered in Resources — only the
//! editable knobs are. Per-project account overrides are read-only here; the "Default
//! account" rows write the WORKSPACE default (`projectId: None`) only, not a per-project
//! pin. The Agent config file editor has no dirty-guard confirm (unlike the IDE's) — `Esc`
//! discards a pending edit outright. Prompt templates carry no `skills` auto-apply list.

use coducktor_contract::{
    AgentConfigFileContent, AgentConfigListing, AgentProfilesResponse, Appearance, ConfigResponse,
    NotificationsUiState, PromptTemplate, QuotaRoutingPatch, Runner, SelectAgentProfileInput,
    SetConfigInput, SetWorkspaceConfigInput, UiState, UpdateAgentProfileInput, UpdateProjectInput,
    WorkspaceConfigResponse, WorkspaceUiState, WorktreesResponse,
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

const RUNNERS: [Runner; 4] = [Runner::Claude, Runner::Codex, Runner::OpenCode, Runner::Pi];
const THEMES: [ThemeName; 3] = [ThemeName::Light, ThemeName::Dark, ThemeName::LazyVim];

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

#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    BaseBranch,
    Model(Runner),
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

    pub config: Option<ConfigResponse>,
    pub workspace_config: Option<WorkspaceConfigResponse>,
    pub workspace_ui_state: Option<WorkspaceUiState>,
    pub ui_state: Option<UiState>,
    pub agent_config: Option<AgentConfigListing>,
    pub agent_profiles: Option<AgentProfilesResponse>,
    pub worktrees: Option<WorktreesResponse>,

    pub open_file: Option<AgentConfigFileContent>,
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
            config: None,
            workspace_config: None,
            workspace_ui_state: None,
            ui_state: None,
            agent_config: None,
            agent_profiles: None,
            worktrees: None,
            open_file: None,
            file_editing: false,
            file_editor: Editor::default(),
            file_highlighter: Highlighter::new(true),
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
    app.settings_ui.edit = None;
    app.settings_ui.file_editing = false;
    app.request_navigate(Route::Settings {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadSettings {
        project: project.to_owned(),
    });
}

fn current_section(app: &App) -> SettingsSection {
    SECTIONS[app.settings_ui.section.min(SECTIONS.len() - 1)]
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
    let Some(config) = &app.settings_ui.config else {
        return vec![row("Loading…", "")];
    };
    vec![
        row("Base branch", opt_str(&config.base_branch)),
        row(
            "Default runner",
            runner_selection_label(config.default_runner),
        ),
        row(
            "Default model — claude",
            opt_str(&config.default_models.claude),
        ),
        row(
            "Default model — codex",
            opt_str(&config.default_models.codex),
        ),
        row(
            "Default model — opencode",
            opt_str(&config.default_models.opencode),
        ),
        row("Default model — pi", opt_str(&config.default_models.pi)),
        row("System prompt", opt_str(&config.system_prompt)),
        row(
            "Live title updates",
            bool_label(config.live_title_updates.unwrap_or(true)),
        ),
        row(
            "Review gate",
            bool_label(config.review_gate.unwrap_or(true)),
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
        row("Theme (this session)", app.theme.name.label().to_owned()),
        row(
            "Accent",
            appearance
                .accent
                .map(|accent| format!("{accent:?}").to_lowercase())
                .unwrap_or_else(|| "lime".to_owned()),
        ),
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
        row("Memory limit (MB)", opt_num(resources.memory_limit_mb)),
        row(
            "Default worktree retention",
            resources.worktree_retention_default.to_string(),
        ),
    ];
    rows.push(row(
        "Quota routing",
        bool_label(config.quota_routing.as_ref().is_some_and(|q| q.enabled)),
    ));
    rows
}

fn rows_projects(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
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
                "max-parallel={}  tags={}",
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
}

fn render_nav(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title("Settings");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_scope = "";
    for (index, section) in SECTIONS.iter().enumerate() {
        if section.scope_label() != current_scope {
            current_scope = section.scope_label();
            lines.push(Line::from(Span::styled(
                current_scope.to_uppercase(),
                Style::default().fg(app.theme.palette.soft_fg),
            )));
        }
        let mut style = Style::default().fg(app.theme.palette.fg);
        if index == app.settings_ui.section {
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
    let block = Block::default().borders(Borders::BOTTOM);
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
    for (index, entry) in rows.iter().enumerate() {
        let selected = index == app.settings_ui.row;
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
        if let Some(y) = inner.y.checked_add(index as u16)
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
    match key.code {
        KeyCode::Tab => {
            app.settings_ui.section = (app.settings_ui.section + 1) % SECTIONS.len();
            app.settings_ui.row = 0;
            true
        }
        KeyCode::BackTab => {
            app.settings_ui.section =
                (app.settings_ui.section + SECTIONS.len() - 1) % SECTIONS.len();
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
        KeyCode::Left => {
            cycle(app, true);
            true
        }
        KeyCode::Right => {
            cycle(app, false);
            true
        }
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
        (SettingsSection::Appearance, index) => cycle_appearance(app, index, backward),
        (SettingsSection::Notifications, 0) => toggle_notifications(app),
        (SettingsSection::Resources, index) => toggle_or_ignore_resource(app, index),
        _ => {}
    }
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
        }
        1 => {
            let mut appearance = current_appearance(app);
            let next = match appearance.accent {
                Some(coducktor_contract::Accent::Lime) | None => coducktor_contract::Accent::Violet,
                Some(coducktor_contract::Accent::Violet) => coducktor_contract::Accent::Lime,
            };
            appearance.accent = Some(next);
            put_appearance(app, appearance);
        }
        2 => {
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
        3 => {
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
        7 => {
            let enabled = !config.quota_routing.as_ref().is_some_and(|q| q.enabled);
            let input = SetWorkspaceConfigInput {
                quota_routing: Some(QuotaRoutingPatch {
                    enabled: Some(enabled),
                }),
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutWorkspaceConfig { input });
            return;
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

fn activate_agents(app: &mut App, row: usize) {
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
        2 => start_edit(
            app,
            EditTarget::Model(Runner::Claude),
            config.default_models.claude.unwrap_or_default(),
        ),
        3 => start_edit(
            app,
            EditTarget::Model(Runner::Codex),
            config.default_models.codex.unwrap_or_default(),
        ),
        4 => start_edit(
            app,
            EditTarget::Model(Runner::OpenCode),
            config.default_models.opencode.unwrap_or_default(),
        ),
        5 => start_edit(
            app,
            EditTarget::Model(Runner::Pi),
            config.default_models.pi.unwrap_or_default(),
        ),
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
        let current = app
            .settings_ui
            .workspace_config
            .as_ref()
            .map(|c| c.projects_dir.clone())
            .unwrap_or_default();
        start_edit(app, EditTarget::ChecksoutRoot, current);
        return;
    }
    let Some(project) = app.project_registry.get(row - 1) else {
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
        SettingsSection::Projects if row > 0 => {
            if let Some(project) = app.project_registry.get(row - 1) {
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
        EditTarget::SystemPrompt => {
            let input = SetConfigInput {
                system_prompt: Some(if text.is_empty() { None } else { Some(text) }),
                ..Default::default()
            };
            app.pending
                .push(PendingAction::SettingsPutConfig { project, input });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use coducktor_contract::{
        AgentDefaults, ComposerDefaults, InheritedAutonomous, RunnerModels, RunnerSelection,
        WorkspaceResources,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_config() -> ConfigResponse {
        ConfigResponse {
            base_branch: None,
            default_runner: RunnerSelection::Auto,
            system_prompt: None,
            default_models: RunnerModels::default(),
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
}
