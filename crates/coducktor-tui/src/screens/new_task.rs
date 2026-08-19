//! New Task screen — `screens/new_task.rs`.
//!
//! Layout: a centered hero with
//! "What should the agent work on?", the shared composer card (auto-growing text
//! area, pasted-image row — no Dictation, per decision 2), a pill row —
//! `skill/workflow ▾` · `runner ▾` · `model ▾` · `reasoning ▾` · `×N variants ▾` ·
//! `branch: <branch> ▾` · `worktree: off ▾` · `mode: autonomous ▾` — then the send hint.

use coducktor_contract::{
    ProviderStatusResponse, ReasoningEffort, RepoInfo, Runner, RunnerModelCatalogResponse,
    RunnerSelection, Skill, TaskSource, UiState, WorkflowDef, WorkspaceConfigResponse,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, PendingAction};
use crate::input::hitmap::{HitAction, NewTaskAction};
use crate::new_task_form::{
    self, ComposerConfig, CreateRunBodyOpts, ModelPreset, NewTaskDraft, ReasoningOption,
};
use crate::theme::Theme;
use crate::widgets::composer::{Composer, ComposerContext, ComposerEvent};
use crate::widgets::picker::{Picker, PickerEvent, PickerItem};

/// The pills on the composer's second row, in focus order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillId {
    Source,
    Runner,
    Model,
    Reasoning,
    Variants,
    Base,
    Worktree,
    Autonomous,
}

impl PillId {
    const ALL: [Self; 8] = [
        Self::Source,
        Self::Runner,
        Self::Model,
        Self::Reasoning,
        Self::Variants,
        Self::Base,
        Self::Worktree,
        Self::Autonomous,
    ];

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|pill| *pill == self).unwrap_or(0)
    }
}

/// The open pill picker, with its candidate list.
#[derive(Debug, Clone)]
pub enum PickerKind {
    Source(Picker),
    Runner(Picker),
    Model(Picker),
    Reasoning(Picker),
    Variants(Picker),
    Base(Picker),
    Worktree(Picker),
    Autonomous(Picker),
}

impl PickerKind {
    fn picker(&self) -> &Picker {
        match self {
            Self::Source(picker)
            | Self::Runner(picker)
            | Self::Model(picker)
            | Self::Reasoning(picker)
            | Self::Variants(picker)
            | Self::Base(picker)
            | Self::Worktree(picker)
            | Self::Autonomous(picker) => picker,
        }
    }

    fn picker_mut(&mut self) -> &mut Picker {
        match self {
            Self::Source(picker)
            | Self::Runner(picker)
            | Self::Model(picker)
            | Self::Reasoning(picker)
            | Self::Variants(picker)
            | Self::Base(picker)
            | Self::Worktree(picker)
            | Self::Autonomous(picker) => picker,
        }
    }
}

/// The per-project data the screen reads.
#[derive(Debug, Clone, Default)]
pub struct NewTaskData {
    pub skills: Vec<Skill>,
    pub workflows: Vec<WorkflowDef>,
    pub config: Option<ComposerConfig>,
    pub workspace_config: Option<WorkspaceConfigResponse>,
    pub provider_status: Option<ProviderStatusResponse>,
    pub ui_state: Option<UiState>,
    pub model_catalog: Option<RunnerModelCatalogResponse>,
    /// The runner the catalog was requested for; a runner change re-requests it.
    pub models_requested_for: Option<Runner>,
    pub repo: Option<RepoInfo>,
    pub branches: Vec<String>,
}

/// Screen-local state: the per-project draft, the shared composer, focus, and the
/// open pill-picker overlay.
#[derive(Debug, Clone, Default)]
pub struct NewTaskUi {
    pub draft: NewTaskDraft,
    pub draft_project: Option<String>,
    pub composer: Composer,
    pub composer_focused: bool,
    pub pill_focus: Option<PillId>,
    pub picker: Option<PickerKind>,
    pub data: NewTaskData,
}

/// The fully-resolved composer values (draft + config + catalogs), mirroring the
/// the derived picker state used by the new-task screen.
pub struct Effective {
    pub source: TaskSource,
    pub runners: Vec<Runner>,
    pub runner: RunnerSelection,
    pub display_runner: Runner,
    pub models: Vec<ModelPreset>,
    pub model: String,
    pub reasoning_options: Vec<ReasoningOption>,
    pub reasoning_effort: ReasoningEffort,
    pub variants: u64,
    pub has_git: bool,
    pub worktree_on: bool,
    pub autonomous_on: bool,
    pub base_branch: String,
    pub providers_ready: bool,
}

pub fn effective_values(draft: &NewTaskDraft, data: &NewTaskData) -> Effective {
    let source = new_task_form::resolve_source(
        &[
            draft.source.clone(),
            data.ui_state
                .as_ref()
                .and_then(|state| state.last_task.clone()),
        ],
        &data.skills,
        &data.workflows,
    );
    let selected_skill = match &source {
        TaskSource::Skill { reference } => data
            .skills
            .iter()
            .find(|skill| skill.name == *reference)
            .cloned(),
        _ => None,
    };

    let runners = new_task_form::usable_runners(data.provider_status.as_ref());
    let preferred = data
        .config
        .as_ref()
        .map(|config| config.default_runner)
        .unwrap_or(RunnerSelection::Claude);
    let runner = if runners.is_empty() {
        preferred
    } else {
        new_task_form::resolve_runner_selection(draft.runner, &runners, preferred)
    };
    let display_runner = match runner {
        RunnerSelection::Auto => Runner::Claude,
        RunnerSelection::Claude => Runner::Claude,
        RunnerSelection::Codex => Runner::Codex,
        RunnerSelection::OpenCode => Runner::OpenCode,
        RunnerSelection::Pi => Runner::Pi,
    };
    let display_catalog = data
        .model_catalog
        .as_ref()
        .filter(|catalog| catalog.runner == display_runner);

    let models = new_task_form::models_for_runner(
        display_runner,
        display_catalog,
        &[
            draft.model.as_deref(),
            data.config
                .as_ref()
                .and_then(|config| match display_runner {
                    Runner::Claude => config.default_models.claude.as_deref(),
                    Runner::Codex => config.default_models.codex.as_deref(),
                    Runner::OpenCode => config.default_models.opencode.as_deref(),
                    Runner::Pi => config.default_models.pi.as_deref(),
                }),
        ],
    );
    let models_locked = data
        .config
        .as_ref()
        .map(|config| config.models_locked)
        .unwrap_or(false);
    let model = if models_locked {
        String::new()
    } else {
        new_task_form::resolve_model(
            draft.model.as_deref(),
            display_runner,
            data.config.as_ref().map(|config| &config.default_models),
            display_catalog,
        )
    };
    let workspace_defaults = data
        .workspace_config
        .as_ref()
        .map(|config| &config.composer_defaults);
    let project_defaults = data
        .config
        .as_ref()
        .and_then(|config| config.composer_defaults.as_ref());
    let reasoning_options = new_task_form::reasoning_options_for_model(&model, &models);
    let configured_reasoning = project_defaults
        .and_then(|defaults| defaults.reasoning)
        .or_else(|| workspace_defaults.and_then(|defaults| defaults.reasoning))
        .unwrap_or(ReasoningEffort::Auto);
    let draft_effort = draft.reasoning_effort.unwrap_or(configured_reasoning);
    let reasoning_effort = if reasoning_options
        .iter()
        .any(|option| option.value == draft_effort)
    {
        draft_effort
    } else {
        ReasoningEffort::Auto
    };

    let has_git = data.repo.is_some();
    let configured_variants = project_defaults
        .and_then(|defaults| defaults.variants)
        .or_else(|| workspace_defaults.and_then(|defaults| defaults.variants))
        .unwrap_or(1)
        .clamp(1, 3);
    let variants = if has_git {
        if draft.variants_explicit || draft.variants != 1 {
            draft.variants.clamp(1, 3)
        } else {
            configured_variants
        }
    } else {
        1
    };
    let configured_autonomous = draft.autonomous.or_else(|| {
        project_defaults
            .and_then(|defaults| defaults.autonomous)
            .or_else(|| {
                workspace_defaults.and_then(|defaults| {
                    defaults.autonomous.or(match defaults.inherited_autonomous {
                        coducktor_contract::InheritedAutonomous::Value(value) => Some(value),
                        coducktor_contract::InheritedAutonomous::SourceDependent => None,
                    })
                })
            })
    });
    let configured_worktree = project_defaults
        .and_then(|defaults| defaults.worktree)
        .or_else(|| {
            workspace_defaults
                .map(|defaults| defaults.worktree.unwrap_or(defaults.inherited_worktree))
        })
        .unwrap_or(false);
    let (autonomous_on, worktree_on) = new_task_form::resolve_composer_run_mode(
        has_git,
        variants,
        draft.worktree,
        selected_skill
            .as_ref()
            .and_then(|skill| skill.interactive)
            .unwrap_or(false),
        configured_autonomous,
        configured_worktree,
    );

    let base_branch = data
        .config
        .as_ref()
        .and_then(|config| config.base_branch.clone())
        .or_else(|| data.repo.as_ref().map(|repo| repo.branch.clone()))
        .unwrap_or_else(|| "main".to_owned());

    Effective {
        providers_ready: !runners.is_empty(),
        source,
        runners,
        runner,
        display_runner,
        models,
        model,
        reasoning_options,
        reasoning_effort,
        variants,
        has_git,
        worktree_on,
        autonomous_on,
        base_branch,
    }
}

fn runner_label(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    }
}

/// Keep the loaded draft in sync with the current project (drafts are per-project).
fn sync_draft(app: &mut App) {
    let project = app.current_project().to_owned();
    if app.new_task_ui.draft_project.as_deref() != Some(project.as_str()) {
        let draft = app
            .new_task_drafts
            .get(&project)
            .cloned()
            .unwrap_or_default();
        if let Some(composer) = app.new_task_composers.get(&project).cloned() {
            app.new_task_ui.composer = composer;
        } else {
            app.new_task_ui.composer = Composer::default();
            app.new_task_ui.composer.set_text(&draft.text);
        }
        app.new_task_ui.draft = draft;
        app.new_task_ui.draft_project = Some(project);
    }
}

fn write_draft(app: &mut App) {
    let project = app.current_project().to_owned();
    app.new_task_drafts
        .insert(project.clone(), app.new_task_ui.draft.clone());
    app.new_task_composers
        .insert(project, app.new_task_ui.composer.clone());
}

fn composer_context<'a>(app: &'a App) -> ComposerContext<'a> {
    let data = &app.new_task_ui.data;
    ComposerContext {
        skills: &data.skills,
        skill_usage: data
            .ui_state
            .as_ref()
            .and_then(|state| state.skill_usage.as_ref()),
        mention_candidates: &[],
    }
}

pub fn apply_hit(app: &mut App, action: NewTaskAction) {
    match action {
        NewTaskAction::SourcePill => open_pill(app, PillId::Source),
        NewTaskAction::RunnerPill => open_pill(app, PillId::Runner),
        NewTaskAction::ModelPill => open_pill(app, PillId::Model),
        NewTaskAction::ReasoningPill => open_pill(app, PillId::Reasoning),
        NewTaskAction::VariantsPill => open_pill(app, PillId::Variants),
        NewTaskAction::BasePill => open_pill(app, PillId::Base),
        NewTaskAction::WorktreePill => open_pill(app, PillId::Worktree),
        NewTaskAction::AutonomousPill => open_pill(app, PillId::Autonomous),
        NewTaskAction::Compose => {
            app.new_task_ui.pill_focus = None;
            app.new_task_ui.composer_focused = true;
            app.new_task_ui.composer.focus();
        }
    }
}

pub fn pick_index(app: &mut App, index: usize) {
    let Some(kind) = app.new_task_ui.picker.as_mut() else {
        return;
    };
    kind.picker_mut().selected = index;
    let picker = kind.picker().clone();
    if let Some(item) = picker.items.get(index) {
        let value = item.value.clone();
        let pill = picker_pill(kind);
        apply_pick(app, pill, &value);
    }
    app.new_task_ui.picker = None;
}

fn picker_pill(kind: &PickerKind) -> PillId {
    match kind {
        PickerKind::Source(_) => PillId::Source,
        PickerKind::Runner(_) => PillId::Runner,
        PickerKind::Model(_) => PillId::Model,
        PickerKind::Reasoning(_) => PillId::Reasoning,
        PickerKind::Variants(_) => PillId::Variants,
        PickerKind::Base(_) => PillId::Base,
        PickerKind::Worktree(_) => PillId::Worktree,
        PickerKind::Autonomous(_) => PillId::Autonomous,
    }
}

pub fn remove_attachment(app: &mut App, index: usize) {
    app.new_task_ui.composer.remove_attachment(index);
}

/// Main.rs calls this after a successful start: the text is spent, choices remain.
pub fn clear_draft(app: &mut App) {
    app.new_task_ui.composer.clear_content();
    app.new_task_ui.draft.text.clear();
    write_draft(app);
}

/// Restore the exact draft captured when a scoped start was submitted. Navigation may have
/// changed the active project by the time the engine reports an error, so the per-project store
/// is updated first and the visible composer is refreshed only when it is still that project.
pub fn restore_start_draft(app: &mut App, project: &str) {
    let Some(draft) = app.pending_start_drafts.remove(project) else {
        return;
    };
    let composer = app.pending_start_composers.remove(project);
    app.new_task_drafts
        .insert(project.to_owned(), draft.clone());
    if let Some(composer) = &composer {
        app.new_task_composers
            .insert(project.to_owned(), composer.clone());
    }
    if app.current_project() == project {
        app.new_task_ui.draft = draft.clone();
        app.new_task_ui.draft_project = Some(project.to_owned());
        if let Some(composer) = composer {
            app.new_task_ui.composer = composer;
        } else {
            app.new_task_ui.composer.set_text(&draft.text);
        }
        app.new_task_ui.composer_focused = true;
        app.new_task_ui.composer.focus();
    }
}

/// The keyboard contract for the New Task screen. Returns true when consumed.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.new_task_ui.picker.is_some() {
        return handle_picker_key(app, key);
    }
    if app.new_task_ui.composer_focused {
        // Keep the widget's visual focus in lockstep with the screen focus flag. A composer can
        // be restored from a per-project draft between frames, so relying on the cloned widget's
        // old `focused` bit made `i` look like it sometimes did nothing.
        app.new_task_ui.composer.focus();
        return handle_composer_key(app, key);
    }
    match key.code {
        KeyCode::Tab => {
            app.new_task_ui.pill_focus = Some(
                app.new_task_ui
                    .pill_focus
                    .map(PillId::next)
                    .unwrap_or(PillId::Source),
            );
            true
        }
        KeyCode::BackTab => {
            app.new_task_ui.pill_focus = Some(
                app.new_task_ui
                    .pill_focus
                    .map(PillId::previous)
                    .unwrap_or(PillId::Autonomous),
            );
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(pill) = app.new_task_ui.pill_focus {
                app.new_task_ui.pill_focus = Some(pill.next());
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(pill) = app.new_task_ui.pill_focus {
                app.new_task_ui.pill_focus = Some(pill.previous());
            }
            true
        }
        KeyCode::Char(' ') if app.new_task_ui.pill_focus == Some(PillId::Autonomous) => {
            toggle_autonomous(app);
            true
        }
        KeyCode::Char(' ') if app.new_task_ui.pill_focus == Some(PillId::Worktree) => {
            toggle_worktree(app);
            true
        }
        KeyCode::Enter if app.new_task_ui.pill_focus.is_some() => {
            if let Some(pill) = app.new_task_ui.pill_focus {
                open_pill(app, pill);
            }
            true
        }
        KeyCode::Char('i') | KeyCode::Enter => {
            app.new_task_ui.pill_focus = None;
            app.new_task_ui.composer_focused = true;
            app.new_task_ui.composer.focus();
            true
        }
        KeyCode::Char('n') | KeyCode::Char('s') => {
            request_start(app);
            true
        }
        _ => false,
    }
}

/// Deliver a bracketed-paste chunk to the focused composer.
pub fn handle_paste(app: &mut App, text: &str) -> bool {
    if !app.new_task_ui.composer_focused || app.new_task_ui.picker.is_some() {
        return false;
    }
    let ctx = composer_context(app);
    let event = {
        let mut composer = app.new_task_ui.composer.clone();
        let event = composer.handle_paste(text, &ctx);
        app.new_task_ui.composer = composer;
        event
    };
    handle_composer_event(app, event)
}

fn handle_composer_key(app: &mut App, key: KeyEvent) -> bool {
    if is_clipboard_paste_key(key) {
        return handle_clipboard_paste(app);
    }
    let ctx = composer_context(app);
    let event = {
        let mut composer = app.new_task_ui.composer.clone();
        let event = composer.handle_key(key, &ctx);
        app.new_task_ui.composer = composer;
        event
    };
    handle_composer_event(app, event)
}

fn handle_composer_event(app: &mut App, event: ComposerEvent) -> bool {
    match event {
        ComposerEvent::Changed => {
            app.new_task_ui.draft.text = app.new_task_ui.composer.text.clone();
            write_draft(app);
            true
        }
        ComposerEvent::Submit { text }
            if text.trim().is_empty() && app.new_task_ui.composer.attachments.is_empty() =>
        {
            app.notice = Some("describe a task first".to_owned());
            true
        }
        ComposerEvent::Submit { .. } => {
            request_start(app);
            true
        }
        ComposerEvent::PickedSkill { name } => {
            bump_skill_usage(app, &name);
            app.new_task_ui.draft.text = app.new_task_ui.composer.text.clone();
            write_draft(app);
            true
        }
        ComposerEvent::Blur => {
            app.new_task_ui.composer_focused = false;
            app.new_task_ui.composer.blur();
            app.new_task_ui.draft.text = app.new_task_ui.composer.text.clone();
            write_draft(app);
            true
        }
    }
}

fn is_clipboard_paste_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(character) if character.eq_ignore_ascii_case(&'v'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn handle_clipboard_paste(app: &mut App) -> bool {
    match crate::clipboard::read() {
        Ok(crate::clipboard::ClipboardContent::ImagePng(png)) => {
            app.new_task_ui.composer.attach_clipboard_image(png);
            handle_composer_event(app, ComposerEvent::Changed)
        }
        Ok(crate::clipboard::ClipboardContent::Text(text)) => handle_paste(app, &text),
        Err(error) => {
            app.notice = Some(format!("Could not paste: {error}"));
            true
        }
    }
}

fn bump_skill_usage(app: &mut App, name: &str) {
    let project = app.current_project().to_owned();
    let mut state = app.new_task_ui.data.ui_state.clone().unwrap_or_default();
    state.skill_usage = Some(crate::skills::bump_skill_usage(
        state.skill_usage.as_ref(),
        name,
    ));
    app.new_task_ui.data.ui_state = Some(state.clone());
    app.pending
        .push(PendingAction::PutUiState { project, state });
}

fn handle_picker_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(kind) = app.new_task_ui.picker.as_mut() else {
        return true;
    };
    let event = kind.picker_mut().handle_key(key);
    match event {
        PickerEvent::Query(_) => {
            refresh_picker_items(app);
            true
        }
        PickerEvent::Select(index) => {
            let (pill, value) = {
                let ui = &app.new_task_ui;
                let Some(kind) = ui.picker.as_ref() else {
                    return true;
                };
                let picker = kind.picker();
                let value = picker.items.get(index).map(|item| item.value.clone());
                (picker_pill(kind), value)
            };
            app.new_task_ui.picker = None;
            if let Some(value) = value {
                apply_pick(app, pill, &value);
            }
            true
        }
        PickerEvent::Close => {
            app.new_task_ui.picker = None;
            true
        }
        PickerEvent::Noop => true,
    }
}

fn toggle_autonomous(app: &mut App) {
    let draft = &mut app.new_task_ui.draft;
    let effective = effective_values(draft, &app.new_task_ui.data);
    draft.autonomous = Some(!effective.autonomous_on);
    write_draft(app);
}

fn toggle_worktree(app: &mut App) {
    let draft = &mut app.new_task_ui.draft;
    let effective = effective_values(draft, &app.new_task_ui.data);
    if effective.variants > 1 {
        return;
    }
    draft.worktree = Some(!effective.worktree_on);
    write_draft(app);
}

fn open_pill(app: &mut App, pill: PillId) {
    let mut picker = Picker::new(picker_title(pill));
    picker.searchable = matches!(pill, PillId::Source | PillId::Base);
    app.new_task_ui.pill_focus = Some(pill);
    app.new_task_ui.picker = Some(match pill {
        PillId::Source => PickerKind::Source(picker),
        PillId::Runner => PickerKind::Runner(picker),
        PillId::Model => PickerKind::Model(picker),
        PillId::Reasoning => PickerKind::Reasoning(picker),
        PillId::Variants => PickerKind::Variants(picker),
        PillId::Base => PickerKind::Base(picker),
        PillId::Worktree => PickerKind::Worktree(picker),
        PillId::Autonomous => PickerKind::Autonomous(picker),
    });
    refresh_picker_items(app);
}

fn picker_title(pill: PillId) -> &'static str {
    match pill {
        PillId::Source => "skill / workflow",
        PillId::Runner => "RUNNER",
        PillId::Model => "MODEL",
        PillId::Reasoning => "REASONING",
        PillId::Variants => "VARIANTS",
        PillId::Base => "BRANCH",
        PillId::Worktree => "WORKTREE",
        PillId::Autonomous => "TASK MODE",
    }
}

/// Apply an asynchronously discovered catalog and update an already-open model picker.
pub fn apply_model_catalog(app: &mut App, catalog: RunnerModelCatalogResponse) {
    app.new_task_ui.data.model_catalog = Some(catalog);
    if matches!(app.new_task_ui.picker, Some(PickerKind::Model(_))) {
        refresh_picker_items(app);
    }
}

/// Recompute the open picker's candidates from its query (the source picker is
/// searchable; the simple pills just re-offer their fixed lists).
fn refresh_picker_items(app: &mut App) {
    let Some(kind) = app.new_task_ui.picker.as_mut() else {
        return;
    };
    let query = kind.picker().query.clone();
    let items = match kind {
        PickerKind::Source(picker) => {
            let usage = app
                .new_task_ui
                .data
                .ui_state
                .as_ref()
                .and_then(|state| state.skill_usage.as_ref());
            let mut items = source_picker_items(&app.new_task_ui.data, &query, usage);
            picker.set_items(std::mem::take(&mut items));
            return;
        }
        PickerKind::Runner(_) => runner_picker_items(&app.new_task_ui.data),
        PickerKind::Model(_) => model_picker_items(&app.new_task_ui.draft, &app.new_task_ui.data),
        PickerKind::Reasoning(_) => {
            reasoning_picker_items(&app.new_task_ui.draft, &app.new_task_ui.data)
        }
        PickerKind::Variants(_) => {
            variants_picker_items(&app.new_task_ui.draft, &app.new_task_ui.data)
        }
        PickerKind::Base(_) => {
            base_picker_items(&app.new_task_ui.draft, &app.new_task_ui.data, &query)
        }
        PickerKind::Worktree(_) => {
            worktree_picker_items(&app.new_task_ui.draft, &app.new_task_ui.data)
        }
        PickerKind::Autonomous(_) => autonomous_picker_items(),
    };
    kind.picker_mut().set_items(items);
}

fn source_picker_items(
    data: &NewTaskData,
    query: &str,
    usage: Option<&std::collections::BTreeMap<String, f64>>,
) -> Vec<PickerItem> {
    let mut items: Vec<PickerItem> = Vec::new();
    let matched = crate::skills::search_skills(&data.skills, query, usage)
        .into_iter()
        .filter(|skill| skill.name != coducktor_core::skills::BUILT_IN_PLANNING_SKILL_NAME)
        .collect::<Vec<_>>();
    let tiers =
        crate::skills::partition_skill_refs(&matched, usage, crate::skills::MOST_USED_LIMIT);
    let query = query.trim().to_lowercase();
    let baseline_matches = "execution baseline no skill plain task".contains(&query);
    if baseline_matches {
        items.push(PickerItem {
            value: "baseline".to_owned(),
            label: "execution".to_owned(),
            description: Some("run the task as written, without a skill or workflow".to_owned()),
            group: Some("task mode".to_owned()),
            emphasized: false,
        });
    }
    if "planning plan".contains(&query) {
        items.push(PickerItem {
            value: format!(
                "skill:{}",
                coducktor_core::skills::BUILT_IN_PLANNING_SKILL_NAME
            ),
            label: coducktor_core::skills::BUILT_IN_PLANNING_SKILL_NAME.to_owned(),
            description: Some(
                coducktor_core::skills::BUILT_IN_PLANNING_SKILL_DESCRIPTION.to_owned(),
            ),
            group: Some("task mode".to_owned()),
            emphasized: false,
        });
    }
    for (group, skills) in [
        ("most used", tiers.most_used),
        ("project skills", tiers.project),
    ] {
        for skill in skills {
            items.push(PickerItem {
                value: format!("skill:{}", skill.name),
                label: skill.name.clone(),
                description: skill.description.clone(),
                group: Some(group.to_owned()),
                emphasized: true,
            });
        }
    }
    let workflows = crate::skills::search_workflows(&data.workflows, &query);
    for workflow in workflows {
        items.push(PickerItem {
            value: format!("workflow:{}", workflow.name),
            label: workflow.name.clone(),
            description: workflow.description.clone(),
            group: Some("workflows".to_owned()),
            emphasized: false,
        });
    }
    for skill in tiers.global {
        items.push(PickerItem {
            value: format!("skill:{}", skill.name),
            label: skill.name.clone(),
            description: skill.description.clone(),
            group: Some("global".to_owned()),
            emphasized: false,
        });
    }
    items
}

fn runner_picker_items(data: &NewTaskData) -> Vec<PickerItem> {
    let effective = effective_values(&NewTaskDraft::default(), data);
    let runners = if effective.runners.is_empty() {
        vec![Runner::Claude]
    } else {
        effective.runners.clone()
    };
    let mut items = Vec::new();
    if runners
        .iter()
        .any(|runner| matches!(runner, Runner::Claude | Runner::Codex | Runner::OpenCode))
    {
        items.push(PickerItem::simple(
            "runner:auto",
            "auto",
            Some("Choose an available provider for this task".to_owned()),
        ));
    }
    items.extend(runners.into_iter().map(|runner| {
        PickerItem::simple(
            format!("runner:{}", runner_label(runner)),
            runner_label(runner).to_owned(),
            Some(runner_desc(runner).to_owned()),
        )
    }));
    items
}

fn runner_desc(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "Claude Code CLI",
        Runner::Codex => "OpenAI Codex (app-server)",
        Runner::OpenCode => "OpenCode (serve)",
        Runner::Pi => "pi CLI (provider/model)",
    }
}

fn model_picker_items(draft: &NewTaskDraft, data: &NewTaskData) -> Vec<PickerItem> {
    let effective = effective_values(draft, data);
    effective
        .models
        .iter()
        .map(|model| {
            PickerItem::simple(
                format!("model:{}", model.id),
                model.label.clone(),
                Some(model.desc.clone()),
            )
        })
        .collect()
}

fn reasoning_picker_items(draft: &NewTaskDraft, data: &NewTaskData) -> Vec<PickerItem> {
    let effective = effective_values(draft, data);
    effective
        .reasoning_options
        .iter()
        .map(|option| {
            PickerItem::simple(
                format!("reasoning:{}", reasoning_id(option.value)),
                option.label.to_owned(),
                Some(option.desc.to_owned()),
            )
        })
        .collect()
}

fn reasoning_id(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Auto => "auto",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "xhigh",
    }
}

fn variants_picker_items(draft: &NewTaskDraft, data: &NewTaskData) -> Vec<PickerItem> {
    let effective = effective_values(draft, data);
    let mut items = vec![
        PickerItem::simple("variants:1", "×1", Some("One run".to_owned())),
        PickerItem::simple(
            "variants:2",
            "×2 variants",
            Some("Two competing runs — pick the diff you keep".to_owned()),
        ),
        PickerItem::simple(
            "variants:3",
            "×3 variants",
            Some("Three competing runs — pick the diff you keep".to_owned()),
        ),
    ];
    if !effective.has_git {
        items.clear();
    }
    items
}

fn base_picker_items(_draft: &NewTaskDraft, data: &NewTaskData, query: &str) -> Vec<PickerItem> {
    let branch = data
        .repo
        .as_ref()
        .map(|repo| repo.branch.clone())
        .unwrap_or_else(|| "main".to_owned());
    let query = query.trim().to_ascii_lowercase();
    let matches = |label: &str| query.is_empty() || label.to_ascii_lowercase().contains(&query);
    let mut items = Vec::new();
    if matches("follow checked-out branch") {
        items.push(PickerItem::simple(
            "base:",
            "follow checked-out branch",
            Some(format!("({branch})")),
        ));
    }
    if data.repo.is_some() {
        let configured = data
            .config
            .as_ref()
            .and_then(|config| config.base_branch.as_ref());
        let mut candidates = Vec::with_capacity(data.branches.len() + 2);
        candidates.push(branch.clone());
        if let Some(configured) = configured
            && !candidates.iter().any(|candidate| candidate == configured)
        {
            candidates.push(configured.clone());
        }
        for candidate in &data.branches {
            if !candidates.iter().any(|existing| existing == candidate) {
                candidates.push(candidate.clone());
            }
        }
        for candidate in candidates {
            if matches(&candidate) {
                items.push(PickerItem::simple(
                    format!("base:{candidate}"),
                    candidate,
                    Some("Task worktrees start from this branch".to_owned()),
                ));
            }
        }
    }
    items
}

fn worktree_picker_items(draft: &NewTaskDraft, data: &NewTaskData) -> Vec<PickerItem> {
    let effective = effective_values(draft, data);
    let mut items = vec![PickerItem::simple(
        "worktree:true",
        "on",
        Some("Run in an isolated task worktree".to_owned()),
    )];
    if effective.variants <= 1 {
        items.push(PickerItem::simple(
            "worktree:false",
            "off",
            Some("Modify the currently checked-out branch directly".to_owned()),
        ));
    }
    items
}

fn autonomous_picker_items() -> Vec<PickerItem> {
    vec![
        PickerItem::simple(
            "autonomous:true",
            "autonomous",
            Some("Continue through the workflow without waiting for approval".to_owned()),
        ),
        PickerItem::simple(
            "autonomous:false",
            "manual approval",
            Some("Pause for your input before continuing or reviewing changes".to_owned()),
        ),
    ]
}

fn apply_pick(app: &mut App, pill: PillId, value: &str) {
    match pill {
        PillId::Source => {
            app.new_task_ui.draft.source = parse_source(value);
        }
        PillId::Runner => {
            if let Some(selection) = parse_runner(value) {
                app.new_task_ui.draft.runner = Some(selection);
                app.new_task_ui.draft.model = None;
                app.new_task_ui.draft.reasoning_effort = None;
            }
        }
        PillId::Model => {
            let id = value.strip_prefix("model:").unwrap_or(value).to_owned();
            app.new_task_ui.draft.model = Some(id);
            app.new_task_ui.draft.reasoning_effort = None;
        }
        PillId::Reasoning => {
            if let Some(effort) = parse_reasoning(value) {
                app.new_task_ui.draft.reasoning_effort = Some(effort);
            }
        }
        PillId::Variants => {
            if let Some(count) = value
                .strip_prefix("variants:")
                .and_then(|count| count.parse().ok())
            {
                app.new_task_ui.draft.variants = count;
                app.new_task_ui.draft.variants_explicit = true;
            }
        }
        PillId::Base => {
            let base_branch = value
                .strip_prefix("base:")
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let project = app.current_project().to_owned();
            app.pending.push(PendingAction::SetBaseBranch {
                project,
                base_branch,
            });
        }
        PillId::Worktree => {
            if let Some(worktree) = value.strip_prefix("worktree:") {
                app.new_task_ui.draft.worktree = Some(worktree == "true");
            }
        }
        PillId::Autonomous => {
            if let Some(autonomous) = value.strip_prefix("autonomous:") {
                app.new_task_ui.draft.autonomous = Some(autonomous == "true");
            }
        }
    }
    write_draft(app);
}

fn parse_source(value: &str) -> Option<TaskSource> {
    match value {
        "baseline" => Some(new_task_form::BASELINE_SOURCE),
        _ => value
            .strip_prefix("skill:")
            .map(|name| TaskSource::Skill {
                reference: name.to_owned(),
            })
            .or_else(|| {
                value
                    .strip_prefix("workflow:")
                    .map(|name| TaskSource::Workflow {
                        reference: name.to_owned(),
                    })
            }),
    }
}

fn parse_runner(value: &str) -> Option<RunnerSelection> {
    match value.strip_prefix("runner:") {
        Some("auto") => Some(RunnerSelection::Auto),
        Some("claude") => Some(RunnerSelection::Claude),
        Some("codex") => Some(RunnerSelection::Codex),
        Some("opencode") => Some(RunnerSelection::OpenCode),
        Some("pi") => Some(RunnerSelection::Pi),
        _ => None,
    }
}

fn parse_reasoning(value: &str) -> Option<ReasoningEffort> {
    match value.strip_prefix("reasoning:") {
        Some("auto") => Some(ReasoningEffort::Auto),
        Some("low") => Some(ReasoningEffort::Low),
        Some("medium") => Some(ReasoningEffort::Medium),
        Some("high") => Some(ReasoningEffort::High),
        Some("xhigh") => Some(ReasoningEffort::XHigh),
        _ => None,
    }
}

fn request_start(app: &mut App) {
    let effective = effective_values(&app.new_task_ui.draft, &app.new_task_ui.data);
    if !effective.providers_ready {
        app.notice = Some("connect an agent provider before starting a task".to_owned());
        return;
    }
    let text = app.new_task_ui.composer.submission_text();
    let images = app.new_task_ui.composer.image_inputs();
    if text.is_empty() && images.is_empty() {
        app.notice = Some("describe a task first".to_owned());
        return;
    }
    let opts = CreateRunBodyOpts {
        task: text,
        source: effective.source.clone(),
        model: effective.model.clone(),
        reasoning_effort: Some(effective.reasoning_effort),
        models_locked: app
            .new_task_ui
            .data
            .config
            .as_ref()
            .map(|config| config.models_locked)
            .unwrap_or(false),
        runner: effective.runner,
        runner_explicit: app.new_task_ui.draft.runner.is_some(),
        default_runner: app
            .new_task_ui
            .data
            .config
            .as_ref()
            .map(|config| config.default_runner),
        variants: effective.variants,
        images,
        worktree: Some(effective.worktree_on),
        autonomous: effective.autonomous_on,
    };
    let input = new_task_form::build_create_run_body(&opts);
    let project = app.current_project().to_owned();
    finish_submit(app, project, input, &effective);
}

/// Queue the start, spend the draft, and remember the source for the next visit.
fn finish_submit(
    app: &mut App,
    project: String,
    input: coducktor_contract::CreateRunInput,
    effective: &Effective,
) {
    app.pending.push(PendingAction::StartRun {
        project: project.clone(),
        input,
    });
    app.pending_start_drafts
        .insert(project.clone(), app.new_task_ui.draft.clone());
    app.pending_start_composers
        .insert(project.clone(), app.new_task_ui.composer.clone());
    // The task now belongs to the background engine operation. Return keyboard focus to the
    // shell immediately so `q` remains a quit key instead of becoming the first character of a
    // second prompt while the agent is starting.
    app.new_task_ui.composer_focused = false;
    app.new_task_ui.composer.focused = false;
    app.new_task_ui.composer.menu = None;
    app.new_task_ui.pill_focus = None;
    clear_draft(app);
    let mut state = app.new_task_ui.data.ui_state.clone().unwrap_or_default();
    state.last_task = Some(effective.source.clone());
    state.recent_sources = Some(new_task_form::push_recent_source(
        state.recent_sources.as_deref(),
        effective.source.clone(),
        24,
    ));
    if let TaskSource::Skill { reference } = &effective.source {
        state.skill_usage = Some(crate::skills::bump_skill_usage(
            state.skill_usage.as_ref(),
            reference,
        ));
    }
    app.new_task_ui.data.ui_state = Some(state.clone());
    app.pending
        .push(PendingAction::PutUiState { project, state });
}

/// Render the whole screen: hero title, composer card, pill row, actions,
/// then the open picker/plan overlays.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    sync_draft(app);
    if app.new_task_ui.composer_focused {
        app.new_task_ui.composer.focus();
    } else {
        app.new_task_ui.composer.blur();
    }
    let theme = app.theme;
    let effective = effective_values(&app.new_task_ui.draft, &app.new_task_ui.data);

    // Model catalog is per-runner; request it once per runner change. A failed
    // catalog degrades to the static presets — never a retry loop.
    if coducktor_contract::runner_discovers_models(effective.display_runner)
        && app.new_task_ui.data.models_requested_for != Some(effective.display_runner)
    {
        app.new_task_ui.data.models_requested_for = Some(effective.display_runner);
        app.queue_pending(PendingAction::RefreshModels {
            runner: effective.display_runner,
        });
    }

    // The composer used to be capped at 72 cells. That made the pill and action
    // rows overflow even on a comfortably wide terminal, and made the narrow
    // layout especially hard to use. Keep a small breathing room at the edges,
    // but let the screen use all of the space it was given.
    let column_width = area.width.saturating_sub(4).max(1);
    let column = Rect::new(
        area.x + area.width.saturating_sub(column_width) / 2,
        area.y,
        column_width,
        area.height,
    );
    let composer_height = app
        .new_task_ui
        .composer
        .height_for_width(column_width.saturating_sub(3))
        + 2;
    let pill_height = pill_row_height(column_width, &effective);
    let action_height = action_row_height(column_width);
    let header = 2;
    let constraints = [
        Constraint::Length(header),
        Constraint::Length(composer_height),
        Constraint::Length(pill_height),
        Constraint::Length(action_height),
        Constraint::Min(1),
    ];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(column);

    // Project context + hero title. This is deliberately non-editable: the pending StartRun
    // already captured the same project id.
    let project = app.current_project().to_owned();
    let root = app
        .project_registry
        .iter()
        .find(|entry| entry.id == project)
        .map(|entry| entry.root.clone())
        .or_else(|| {
            app.boot_root
                .as_ref()
                .map(|root| root.display().to_string())
        })
        .unwrap_or_else(|| "root unavailable".to_owned());
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                format!("New task in {project} · {root}"),
                Style::default().fg(theme.palette.accent),
            )),
            Line::from(Span::styled(
                "What should the agent work on?",
                Style::default()
                    .fg(theme.palette.fg)
                    .add_modifier(Modifier::BOLD),
            )),
        ]))
        .alignment(ratatui::layout::Alignment::Center)
        .style(Style::default().bg(theme.palette.bg)),
        rows[0],
    );

    // The composer card.
    let composer_area = rows[1];
    app.new_task_ui
        .composer
        .render(frame, composer_area, theme, &mut app.hitmap, 5);
    app.hitmap.register(
        composer_area,
        4,
        HitAction::NewTaskScreen(NewTaskAction::Compose),
    );

    // Pill row.
    render_pills(frame, rows[2], app, &effective);

    // Submission hint.
    render_actions(frame, rows[3], app);

    // Overlay: open picker.
    if app.new_task_ui.picker.is_some() {
        render_picker_overlay(frame, area, app);
    }
}

fn render_pills(frame: &mut Frame<'_>, area: Rect, app: &mut App, effective: &Effective) {
    let theme = app.theme;
    let pills = pill_entries(effective);
    let rows = layout_pills(&pills, area.width);
    for (row_index, row) in rows.into_iter().enumerate() {
        let Some(y) = area.y.checked_add(row_index as u16) else {
            continue;
        };
        if y >= area.bottom() {
            break;
        }
        let mut spans = Vec::new();
        let mut column = area.x;
        for (pill, label) in row {
            let focused = app.new_task_ui.pill_focus == Some(pill);
            let style = if focused {
                Style::default()
                    .fg(theme.palette.bg)
                    .bg(theme.palette.accent)
            } else {
                Style::default().fg(theme.palette.soft_fg)
            };
            let width = (label.chars().count() + 4) as u16;
            let display = format!("{label} ▾");
            spans.push(Span::styled(format!(" {display} "), style));
            app.hitmap.register(
                Rect::new(column, y, width.min(area.right().saturating_sub(column)), 1),
                6,
                HitAction::NewTaskScreen(match pill {
                    PillId::Source => NewTaskAction::SourcePill,
                    PillId::Runner => NewTaskAction::RunnerPill,
                    PillId::Model => NewTaskAction::ModelPill,
                    PillId::Reasoning => NewTaskAction::ReasoningPill,
                    PillId::Variants => NewTaskAction::VariantsPill,
                    PillId::Base => NewTaskAction::BasePill,
                    PillId::Worktree => NewTaskAction::WorktreePill,
                    PillId::Autonomous => NewTaskAction::AutonomousPill,
                }),
            );
            column = column.saturating_add(width);
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.palette.bg)),
            Rect::new(area.x, y, area.width, 1),
        );
    }
}

fn pill_entries(effective: &Effective) -> Vec<(PillId, String)> {
    let mut pills = vec![
        (
            PillId::Source,
            match &effective.source {
                TaskSource::Baseline => "source: execution".to_owned(),
                TaskSource::Skill { reference } => format!("source: skill:{reference}"),
                TaskSource::Workflow { reference } => format!("source: workflow:{reference}"),
            },
        ),
        (
            PillId::Runner,
            format!("runner: {}", runner_selection_label(effective.runner)),
        ),
        (
            PillId::Model,
            if effective.model.is_empty() {
                "model: auto".to_owned()
            } else {
                format!("model: {}", effective.model)
            },
        ),
        (
            PillId::Reasoning,
            format!("reasoning: {}", reasoning_label(effective.reasoning_effort)),
        ),
        (
            PillId::Variants,
            format!("variants: ×{}", effective.variants),
        ),
        (PillId::Base, format!("branch: {}", effective.base_branch)),
        (
            PillId::Worktree,
            format!(
                "worktree: {}",
                if effective.worktree_on { "on" } else { "off" }
            ),
        ),
    ];
    let mode = if effective.autonomous_on {
        "autonomous"
    } else {
        "manual approval"
    };
    pills.push((PillId::Autonomous, format!("mode: {mode}")));
    pills
}

fn layout_pills(pills: &[(PillId, String)], width: u16) -> Vec<Vec<(PillId, String)>> {
    let width = usize::from(width.max(1));
    let mut rows: Vec<Vec<(PillId, String)>> = Vec::new();
    let mut row = Vec::new();
    let mut row_width = 0_usize;
    for (pill, label) in pills {
        let item_width = label.chars().count() + 4;
        if !row.is_empty() && row_width + item_width > width {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        row_width += item_width;
        row.push((*pill, label.clone()));
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn pill_row_height(width: u16, effective: &Effective) -> u16 {
    layout_pills(&pill_entries(effective), width).len().max(1) as u16
}

fn action_row_height(width: u16) -> u16 {
    let content = " Enter sends · Esc leaves the composer";
    wrapped_height(content, width)
}

fn wrapped_height(content: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    content.chars().count().div_ceil(width).max(1) as u16
}

fn render_actions(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let line = Line::from(Span::styled(
        " Enter sends · Esc leaves the composer",
        theme_soft(theme),
    ));
    frame.render_widget(
        Paragraph::new(line)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(theme.palette.bg)),
        area,
    );
}

fn theme_soft(theme: Theme) -> Style {
    Style::default().fg(theme.palette.soft_fg)
}

fn runner_selection_label(selection: RunnerSelection) -> String {
    match selection {
        RunnerSelection::Auto => "auto".to_owned(),
        RunnerSelection::Claude => "claude".to_owned(),
        RunnerSelection::Codex => "codex".to_owned(),
        RunnerSelection::OpenCode => "opencode".to_owned(),
        RunnerSelection::Pi => "pi".to_owned(),
    }
}

fn reasoning_label(effort: ReasoningEffort) -> String {
    match effort {
        ReasoningEffort::Auto => "auto".to_owned(),
        ReasoningEffort::Low => "Low".to_owned(),
        ReasoningEffort::Medium => "Medium".to_owned(),
        ReasoningEffort::High => "High".to_owned(),
        ReasoningEffort::XHigh => "Max".to_owned(),
    }
}

fn render_picker_overlay(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(kind) = app.new_task_ui.picker.as_ref() else {
        return;
    };
    let picker = kind.picker();
    let theme = app.theme;
    let hitmap = &mut app.hitmap;
    picker.render(frame, area, theme, hitmap);
}

#[cfg(test)]
mod tests {
    use coducktor_contract::{
        AgentDefaults, ComposerDefaults, InheritedAutonomous, ProjectComposerDefaults,
        ProviderConnectionState, ProviderStatus, RepoInfo, RunStatus, RunnerModelCatalogResponse,
        WorkspaceConfigResponse, WorkspaceResources,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::App;
    use crate::input::keymap::Keymap;

    fn skill(name: &str, source: coducktor_contract::SkillSource) -> Skill {
        Skill {
            name: name.to_owned(),
            description: None,
            interactive: None,
            body: String::new(),
            path: format!("/skills/{name}.md"),
            source,
            team: None,
        }
    }

    fn workflow(name: &str) -> WorkflowDef {
        WorkflowDef {
            name: name.to_owned(),
            description: None,
            steps: Vec::new(),
            source: coducktor_contract::WorkflowSource::BuiltIn,
            path: None,
        }
    }

    fn connected_provider(runner: Runner) -> ProviderStatus {
        ProviderStatus {
            provider: runner,
            status: ProviderConnectionState::Connected,
            enabled: Some(true),
            hint: None,
            auth_failure_id: None,
            profile_id: None,
        }
    }

    fn open_new_task(app: &mut App, project: &str) {
        app.navigate_route(crate::app::Route::NewTask {
            project: project.to_owned(),
        });
        app.new_task_ui.composer_focused = true;
        app.new_task_ui.composer.focus();
    }

    fn app_with_new_task(project: &str) -> App {
        let mut app = App::new(project, Theme::detect(), Keymap::default());
        open_new_task(&mut app, project);
        app.new_task_ui.data.skills = vec![
            skill("om-fix", coducktor_contract::SkillSource::Ai),
            skill("om-open-pr", coducktor_contract::SkillSource::Global),
        ];
        app.new_task_ui.data.workflows = vec![workflow("quick-task")];
        app.new_task_ui.data.provider_status = Some(ProviderStatusResponse {
            providers: vec![connected_provider(Runner::Claude)],
        });
        app.new_task_ui.data.repo = Some(RepoInfo {
            root: "/repo".to_owned(),
            branch: "main".to_owned(),
            remote: None,
        });
        app.new_task_ui.data.branches = vec!["main".to_owned(), "feature/login".to_owned()];
        app.new_task_ui.data.config = Some(ComposerConfig::default());
        app
    }

    fn key(character: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    fn enter_key() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    fn render_app(app: &mut App, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn submitting_a_task_queues_start_run_with_the_expected_body() {
        let mut app = app_with_new_task("t-1");
        assert!(
            app.new_task_ui.composer_focused,
            "hero auto-focuses the composer"
        );
        for character in "ship the shell".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        assert_eq!(app.new_task_ui.draft.text, "ship the shell");
        app.handle_event(crossterm::event::Event::Key(enter_key()));

        let started = app
            .pending
            .iter()
            .find(|action| matches!(action, PendingAction::StartRun { .. }));
        let PendingAction::StartRun { project, input } = started.expect("a start is queued") else {
            unreachable!()
        };
        assert_eq!(project, "t-1");
        let json = serde_json::to_value(input).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "task": "ship the shell",
                "steps": [{ "id": "task", "name": "execution", "prompt": "{{task}}" }],
                "worktree": false,
                "autonomous": true,
            })
        );
        // The text is spent; the source is remembered for the next visit.
        assert!(app.new_task_ui.composer.text.is_empty());
        assert!(!app.new_task_ui.composer_focused);
        assert!(!app.new_task_ui.composer.focused);
        app.handle_event(crossterm::event::Event::Key(key('q')));
        assert!(
            app.should_quit(),
            "q must remain available while start is pending"
        );
        assert!(
            app.pending
                .iter()
                .any(|action| matches!(action, PendingAction::PutUiState { .. }))
        );
    }

    #[test]
    fn pressing_i_focuses_the_composer_without_inserting_i() {
        let mut app = app_with_new_task("t-focus");
        app.new_task_ui.composer.focused = false;
        app.new_task_ui.composer_focused = false;
        assert!(handle_key(&mut app, key('i')));
        assert!(app.new_task_ui.composer.focused);
        assert!(app.new_task_ui.composer.text.is_empty());
        app.new_task_ui.composer.blur();
        let _ = render(&mut app, 120, 40);
        assert!(app.new_task_ui.composer.focused);
    }

    #[test]
    fn workspace_composer_defaults_seed_a_fresh_task() {
        let mut app = app_with_new_task("t-defaults");
        app.new_task_ui.data.workspace_config = Some(WorkspaceConfigResponse {
            projects_dir: "/tmp/projects".to_owned(),
            composer_defaults: ComposerDefaults {
                reasoning: Some(ReasoningEffort::XHigh),
                variants: Some(3),
                autonomous: Some(false),
                worktree: Some(true),
                inherited_autonomous: InheritedAutonomous::Value(true),
                inherited_worktree: false,
            },
            resources: WorkspaceResources {
                max_parallel: 1,
                max_monitoring_sessions: 1,
                monitoring_wake_interval_minutes: None,
                auto_resume_on_usage_limit: false,
                intelligent_context_refresh: false,
                memory_limit_mb: None,
                worktree_retention_default: 0,
            },
            quota_routing: None,
            agent_defaults: AgentDefaults::default(),
        });
        let effective = effective_values(&app.new_task_ui.draft, &app.new_task_ui.data);
        assert_eq!(effective.reasoning_effort, ReasoningEffort::XHigh);
        assert_eq!(effective.variants, 3);
        assert!(!effective.autonomous_on);
        assert!(effective.worktree_on);
    }

    #[test]
    fn project_composer_defaults_override_only_their_global_fields() {
        let mut app = app_with_new_task("t-project-defaults");
        app.new_task_ui.data.config = Some(ComposerConfig {
            composer_defaults: Some(ProjectComposerDefaults {
                reasoning: Some(ReasoningEffort::Low),
                variants: None,
                autonomous: Some(false),
                worktree: None,
            }),
            ..ComposerConfig::default()
        });
        app.new_task_ui.data.workspace_config = Some(WorkspaceConfigResponse {
            projects_dir: "/tmp/projects".to_owned(),
            composer_defaults: ComposerDefaults {
                reasoning: Some(ReasoningEffort::XHigh),
                variants: Some(3),
                autonomous: Some(true),
                worktree: Some(true),
                inherited_autonomous: InheritedAutonomous::Value(true),
                inherited_worktree: false,
            },
            resources: WorkspaceResources {
                max_parallel: 1,
                max_monitoring_sessions: 1,
                monitoring_wake_interval_minutes: None,
                auto_resume_on_usage_limit: false,
                intelligent_context_refresh: false,
                memory_limit_mb: None,
                worktree_retention_default: 0,
            },
            quota_routing: None,
            agent_defaults: AgentDefaults::default(),
        });

        let effective = effective_values(&app.new_task_ui.draft, &app.new_task_ui.data);
        assert_eq!(effective.reasoning_effort, ReasoningEffort::Low);
        assert_eq!(effective.variants, 3);
        assert!(!effective.autonomous_on);
        assert!(effective.worktree_on);
    }

    #[test]
    fn bracketed_paste_updates_the_new_task_draft() {
        let mut app = app_with_new_task("t-paste");
        app.handle_event(crossterm::event::Event::Paste("first\nsecond".to_owned()));

        assert_eq!(app.new_task_ui.composer.text, "first\nsecond");
        assert_eq!(app.new_task_ui.draft.text, "first\nsecond");
    }

    #[test]
    fn large_text_and_clipboard_image_expand_into_the_start_request() {
        let mut app = app_with_new_task("t-rich-paste");
        let pasted = "x".repeat(crate::widgets::composer::LARGE_PASTE_CHAR_THRESHOLD + 1);
        handle_paste(&mut app, &pasted);
        app.new_task_ui
            .composer
            .attach_clipboard_image(vec![1, 2, 3]);

        app.handle_event(crossterm::event::Event::Key(enter_key()));

        let Some(PendingAction::StartRun { input, .. }) = app
            .pending
            .iter()
            .find(|action| matches!(action, PendingAction::StartRun { .. }))
        else {
            panic!("a start is queued");
        };
        assert_eq!(input.task, pasted);
        assert_eq!(input.images.as_ref().map(Vec::len), Some(1));
        assert_eq!(input.images.as_ref().unwrap()[0].data, "AQID");
    }

    #[test]
    fn submitting_with_a_skill_source_builds_the_inline_chain_and_bumps_usage() {
        let mut app = app_with_new_task("t-2");
        app.new_task_ui.draft.source = Some(coducktor_contract::TaskSource::Skill {
            reference: "om-fix".to_owned(),
        });
        for character in "fix the flake".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        app.handle_event(crossterm::event::Event::Key(enter_key()));

        let PendingAction::StartRun { input, .. } = app
            .pending
            .iter()
            .find(|action| matches!(action, PendingAction::StartRun { .. }))
            .expect("a start is queued")
        else {
            unreachable!()
        };
        let json = serde_json::to_value(input).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "task": "fix the flake",
                "steps": [{ "id": "task", "name": "om-fix", "skill": "om-fix", "prompt": "{{task}}" }],
                "worktree": false,
                "autonomous": true,
            })
        );
        // The ui-state write remembers the skill pick AND bumps its usage count.
        let PendingAction::PutUiState { state, .. } = app
            .pending
            .iter()
            .find(|action| matches!(action, PendingAction::PutUiState { .. }))
            .expect("a ui-state write is queued")
        else {
            unreachable!()
        };
        assert_eq!(
            state
                .skill_usage
                .as_ref()
                .and_then(|usage| usage.get("om-fix")),
            Some(&1.0)
        );
        assert_eq!(
            state.last_task.as_ref().unwrap(),
            &coducktor_contract::TaskSource::Skill {
                reference: "om-fix".to_owned(),
            }
        );
    }

    #[test]
    fn empty_or_provider_less_submits_refuse_with_a_notice() {
        let mut app = app_with_new_task("t-3");
        app.handle_event(crossterm::event::Event::Key(enter_key()));
        assert!(
            app.pending
                .iter()
                .all(|action| !matches!(action, PendingAction::StartRun { .. })),
            "empty draft must not start"
        );
        assert!(app.notice.is_some());

        let mut app = app_with_new_task("t-4");
        app.new_task_ui.data.provider_status = None;
        for character in "ship".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        app.handle_event(crossterm::event::Event::Key(enter_key()));
        assert!(
            app.pending
                .iter()
                .all(|action| !matches!(action, PendingAction::StartRun { .. }))
        );
        assert!(app.notice.as_deref().unwrap().contains("provider"));
    }

    #[test]
    fn source_picker_groups_and_ranks_skills() {
        let mut app = app_with_new_task("t-5");
        let mut usage = std::collections::BTreeMap::new();
        usage.insert("om-open-pr".to_owned(), 3.0);
        app.new_task_ui.data.ui_state = Some(UiState {
            skill_usage: Some(usage),
            ..UiState::default()
        });
        let items = source_picker_items(
            &app.new_task_ui.data,
            "",
            app.new_task_ui
                .data
                .ui_state
                .as_ref()
                .and_then(|state| state.skill_usage.as_ref()),
        );
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        // Task modes lead for an empty query, then most used across localities, then project,
        // workflows, and global entries.
        assert_eq!(
            labels,
            [
                "execution",
                "planning",
                "om-open-pr",
                "om-fix",
                "quick-task"
            ]
        );

        let tiers: Vec<&str> = items
            .iter()
            .map(|item| item.group.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(
            tiers,
            [
                "task mode",
                "task mode",
                "most used",
                "project skills",
                "workflows"
            ]
        );

        // A typed query narrows and re-ranks: an exact name match beats the rest.
        let filtered = source_picker_items(&app.new_task_ui.data, "om-fix", None);
        let labels: Vec<&str> = filtered.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["om-fix"]);
    }

    #[test]
    fn effective_values_resolve_defaults_from_the_catalogs() {
        let app = app_with_new_task("t-6");
        let effective = effective_values(&app.new_task_ui.draft, &app.new_task_ui.data);
        assert!(effective.providers_ready);
        assert_eq!(effective.runner, RunnerSelection::Claude);
        assert_eq!(effective.display_runner, Runner::Claude);
        assert_eq!(effective.model, "", "untouched model resolves to auto");
        assert!(effective.has_git);
        assert_eq!(effective.variants, 1);
        assert!(
            effective.autonomous_on,
            "autonomous is the zero-config default"
        );
        assert!(
            !effective.worktree_on,
            "in-place is the zero-config default"
        );
        assert_eq!(effective.base_branch, "main");
    }

    #[test]
    fn branch_picker_lists_the_checked_out_and_available_branches() {
        let mut app = app_with_new_task("t-branch");
        open_pill(&mut app, PillId::Base);
        let items = app
            .new_task_ui
            .picker
            .as_ref()
            .map(|kind| kind.picker().items.clone())
            .unwrap_or_default();
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["follow checked-out branch", "main", "feature/login"]
        );
        assert!(items.iter().all(|item| !item.label.contains("skill")));
    }

    #[test]
    fn worktree_picker_separates_isolation_from_branch_selection() {
        let mut app = app_with_new_task("t-worktree");
        open_pill(&mut app, PillId::Worktree);
        let items = app
            .new_task_ui
            .picker
            .as_ref()
            .map(|kind| kind.picker().items.clone())
            .unwrap_or_default();
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["on", "off"]
        );
        pick_index(&mut app, 1);
        assert_eq!(app.new_task_ui.draft.worktree, Some(false));
        assert!(!effective_values(&app.new_task_ui.draft, &app.new_task_ui.data).worktree_on);
    }

    #[test]
    fn task_mode_picker_has_only_autonomous_and_manual_approval() {
        let mut app = app_with_new_task("t-mode");
        open_pill(&mut app, PillId::Autonomous);
        let items = app
            .new_task_ui
            .picker
            .as_ref()
            .map(|kind| kind.picker().items.clone())
            .unwrap_or_default();
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["autonomous", "manual approval"]
        );
        assert!(items.iter().all(|item| !item.value.starts_with("skill:")));
        pick_index(&mut app, 1);
        assert_eq!(app.new_task_ui.draft.autonomous, Some(false));
        assert!(!effective_values(&app.new_task_ui.draft, &app.new_task_ui.data).autonomous_on);
    }

    #[test]
    fn model_picker_is_keyboard_selectable_for_the_active_runner() {
        let mut app = app_with_new_task("t-model");
        open_pill(&mut app, PillId::Model);
        let target = app
            .new_task_ui
            .picker
            .as_ref()
            .and_then(|kind| {
                kind.picker()
                    .items
                    .iter()
                    .position(|item| item.value == "model:sonnet")
            })
            .expect("Claude's sonnet model should be in the picker");
        for _ in 0..target {
            handle_picker_key(
                &mut app,
                crossterm::event::KeyEvent::new(
                    KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ),
            );
        }
        handle_picker_key(&mut app, enter_key());
        assert_eq!(app.new_task_ui.draft.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn model_picker_uses_the_discovered_catalog_after_switching_to_codex() {
        let mut app = app_with_new_task("t-codex-model");
        app.new_task_ui.data.provider_status = Some(ProviderStatusResponse {
            providers: vec![
                connected_provider(Runner::Claude),
                connected_provider(Runner::Codex),
            ],
        });
        app.new_task_ui.data.model_catalog = Some(RunnerModelCatalogResponse {
            runner: Runner::Codex,
            models: vec![coducktor_contract::RunnerModelOption {
                id: "gpt-5.5-codex".to_owned(),
                label: "GPT 5.5 Codex".to_owned(),
                description: "discovered from Codex".to_owned(),
                reasoning_efforts: None,
            }],
            source: coducktor_contract::ModelCatalogSource::Live,
            stale: false,
            reason: None,
        });
        app.new_task_ui.draft.runner = Some(RunnerSelection::Codex);
        open_pill(&mut app, PillId::Model);
        let items = app
            .new_task_ui
            .picker
            .as_ref()
            .map(|kind| kind.picker().items.clone())
            .unwrap_or_default();
        assert!(items.iter().any(|item| item.value == "model:gpt-5.5-codex"));
        let selected = items
            .iter()
            .position(|item| item.value == "model:gpt-5.5-codex")
            .expect("discovered Codex model should be selectable");
        pick_index(&mut app, selected);
        assert_eq!(
            app.new_task_ui.draft.model.as_deref(),
            Some("gpt-5.5-codex")
        );
    }

    #[test]
    fn open_codex_model_picker_refreshes_when_catalog_arrives() {
        let mut app = app_with_new_task("t-codex-model-refresh");
        app.new_task_ui.data.provider_status = Some(ProviderStatusResponse {
            providers: vec![
                connected_provider(Runner::Claude),
                connected_provider(Runner::Codex),
            ],
        });
        app.new_task_ui.draft.runner = Some(RunnerSelection::Codex);
        open_pill(&mut app, PillId::Model);
        assert_eq!(
            app.new_task_ui
                .picker
                .as_ref()
                .map(|kind| kind.picker().items.len()),
            Some(1)
        );

        apply_model_catalog(
            &mut app,
            RunnerModelCatalogResponse {
                runner: Runner::Codex,
                models: vec![coducktor_contract::RunnerModelOption {
                    id: "gpt-5.6-sol".to_owned(),
                    label: "GPT-5.6-Sol".to_owned(),
                    description: "discovered from Codex".to_owned(),
                    reasoning_efforts: None,
                }],
                source: coducktor_contract::ModelCatalogSource::Live,
                stale: false,
                reason: None,
            },
        );

        let items = app
            .new_task_ui
            .picker
            .as_ref()
            .map(|kind| kind.picker().items.clone())
            .unwrap_or_default();
        assert!(items.iter().any(|item| item.value == "model:gpt-5.6-sol"));
    }

    #[test]
    fn auto_runner_does_not_request_a_non_discovery_claude_catalog() {
        let mut app = app_with_new_task("t-auto-model");
        app.new_task_ui.data.provider_status = Some(ProviderStatusResponse {
            providers: vec![
                connected_provider(Runner::Claude),
                connected_provider(Runner::Codex),
            ],
        });
        app.new_task_ui.data.config = Some(ComposerConfig {
            default_runner: RunnerSelection::Auto,
            ..ComposerConfig::default()
        });
        app.pending.clear();
        render_app(&mut app, 120, 40);
        assert!(
            app.pending
                .iter()
                .all(|action| !matches!(action, PendingAction::RefreshModels { .. }))
        );
    }

    #[test]
    fn runner_picker_offers_auto_before_connected_providers() {
        let data = NewTaskData {
            provider_status: Some(ProviderStatusResponse {
                providers: vec![connected_provider(Runner::Codex)],
            }),
            ..NewTaskData::default()
        };
        let items = runner_picker_items(&data);
        assert_eq!(
            items.first().map(|item| item.value.as_str()),
            Some("runner:auto")
        );
        assert!(items.iter().any(|item| item.value == "runner:codex"));
    }

    #[test]
    fn narrow_new_task_layout_wraps_everything_under_the_composer() {
        let mut app = app_with_new_task("t-layout");
        let content = render(&mut app, 80, 24);
        assert!(
            content.contains("autonomous"),
            "pill row must not clip its final field"
        );
        assert!(
            content.contains("Esc leaves the composer"),
            "action help must remain visible"
        );
        assert!(!content.contains("Fix a failing or flaky test"));
    }

    #[test]
    fn the_source_pill_picker_selects_a_skill_into_the_draft() {
        let mut app = app_with_new_task("t-7");
        open_pill(&mut app, PillId::Source);
        assert!(app.new_task_ui.picker.is_some());
        app.new_task_ui
            .picker
            .as_mut()
            .unwrap()
            .picker_mut()
            .selected = 2;
        let kind = app.new_task_ui.picker.clone().unwrap();
        let value = kind.picker().items[2].value.clone();
        apply_pick(&mut app, PillId::Source, &value);
        assert_eq!(
            app.new_task_ui.draft.source,
            Some(coducktor_contract::TaskSource::Skill {
                reference: "om-fix".to_owned(),
            })
        );
    }

    #[test]
    fn a_started_run_reaches_the_tasks_table_via_the_workspace_stream() {
        let mut app = app_with_new_task("t-8");
        for character in "ship the shell".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        app.handle_event(crossterm::event::Event::Key(enter_key()));
        assert!(
            app.pending
                .iter()
                .any(|action| matches!(action, PendingAction::StartRun { .. }))
        );

        // The workspace run event carries the queued run, and the shell's own task list picks it
        // up in place.
        let project = app.current_project().to_owned();
        let run = crate::app::WorkspaceEvent::Run {
            project: project.clone(),
            run: coducktor_contract::ApiRun {
                record: coducktor_contract::RunRecord {
                    id: "run-1".to_owned(),
                    title: "ship the shell".to_owned(),
                    workflow: "quick-task".to_owned(),
                    task: String::new(),
                    status: RunStatus::Queued,
                    created_at: "2026-08-15T00:00:00Z".to_owned(),
                    tokens_used: 0.0,
                    archived: false,
                    seen_at: None,
                    steps: Vec::new(),
                    ..coducktor_contract::RunRecord::default()
                },
                usage: None,
            },
        };
        app.apply_workspace_event(run);
        assert!(app.tasks.iter().any(|run| run.record.id == "run-1"));

        app.navigate_route(crate::app::Route::Tasks { project });
        let content = render(&mut app, 160, 30);
        assert!(
            content.contains("ship the shell"),
            "the row must appear, got: {content}"
        );
    }

    #[test]
    fn drafts_survive_switching_projects() {
        let mut app = app_with_new_task("t-9");
        for character in "half typed".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        assert_eq!(app.new_task_ui.draft.text, "half typed");

        // Navigate away and back: the draft store restores the text.
        app.navigate_route(crate::app::Route::Tasks {
            project: "main".to_owned(),
        });
        open_new_task(&mut app, "t-0");
        assert_eq!(app.new_task_ui.draft.text, "half typed");
        assert_eq!(app.new_task_ui.composer.text, "half typed");
    }

    #[test]
    fn snapshot_new_task_at_three_sizes() {
        let mut app = app_with_new_task("t-10");
        app.new_task_ui.data.model_catalog = Some(RunnerModelCatalogResponse {
            runner: Runner::Claude,
            models: Vec::new(),
            source: coducktor_contract::ModelCatalogSource::Live,
            stale: false,
            reason: None,
        });
        for character in "ship the shell".chars() {
            app.handle_event(crossterm::event::Event::Key(key(character)));
        }
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("new_task_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
        let _ = render_app;
    }
}
