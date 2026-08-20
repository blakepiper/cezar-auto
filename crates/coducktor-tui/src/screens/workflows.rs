//! The Workflows screen: saved workflows, ordered steps, skill insertion, and YAML
//! import/export/save/delete. The save path preserves the compact `skills:` form when every
//! step is a plain skill step.

use coducktor_contract::{Skill, WorkflowDef, WorkflowSource, WorkflowStepDef};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, PendingAction, Route};
use crate::widgets::editor::Editor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowFocus {
    Tabs,
    Steps,
    Palette,
}

/// Engine-fetched state for the open Workflows screen.
pub struct WorkflowsUi {
    pub project: String,
    pub workflows: Vec<WorkflowDef>,
    /// Index into `workflows`; `workflows.len()` is the "+ new" tab.
    pub selected_tab: usize,

    /// The draft chain being built on the "+ new" tab.
    pub draft_name: String,
    pub draft_steps: Vec<WorkflowStepDef>,
    /// The step cursor — `j`/`k` move it, `Alt+j`/`Alt+k` move the step it points at.
    pub steps_selected: usize,
    /// The row pressed for a drag-reorder (press on a step, release on another) — the
    /// dnd-kit drag's mouse twin. `None` when no drag is in flight.
    pub drag_source: Option<usize>,
    pub name_open: bool,

    /// The skills palette.
    pub palette_skills: Vec<Skill>,
    pub palette_query: String,
    /// Index into `palette_skills` for the selected visible skill.
    pub palette_selected: usize,

    pub focus: WorkflowFocus,
    pub import_open: bool,
    pub import_yaml: Editor,
    pub delete_confirm: Option<String>,
    pub highlighter: crate::diff::Highlighter,
}

impl Default for WorkflowsUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            workflows: Vec::new(),
            selected_tab: 0,
            draft_name: String::new(),
            draft_steps: Vec::new(),
            steps_selected: 0,
            drag_source: None,
            name_open: false,
            palette_skills: Vec::new(),
            palette_query: String::new(),
            palette_selected: 0,
            focus: WorkflowFocus::Tabs,
            import_open: false,
            import_yaml: Editor::default(),
            delete_confirm: None,
            highlighter: crate::diff::Highlighter::new(),
        }
    }
}

/// `skill_stack_of` — the inverse of `skills_to_steps`: when every step is a plain "apply this
/// skill" agent step, return the
/// skill list so the file can be written in the portable compact `skills:` form. Anything
/// richer (checks, custom prompts, per-step models/tools, loops) returns `None`.
pub fn skill_stack_of(steps: &[WorkflowStepDef]) -> Option<Vec<String>> {
    let mut skills = Vec::new();
    for step in steps {
        let skill = step.skill.as_deref()?;
        if step.command.is_some() {
            return None;
        }
        if step
            .prompt
            .as_deref()
            .is_some_and(|prompt| prompt != "{{task}}")
        {
            return None;
        }
        if step.name.as_deref().is_some_and(|name| name != skill) {
            return None;
        }
        if step.model.is_some()
            || step.runner.is_some()
            || step.allowed_tools.is_some()
            || step.bash_allowlist.is_some()
            || step.on_fail.is_some()
        {
            return None;
        }
        skills.push(skill.to_string());
    }
    if skills.is_empty() {
        None
    } else {
        Some(skills)
    }
}

/// One `{{task}}` skill step with a deduplicated id — the palette's append shape.
pub fn skill_step(name: &str, existing: &[WorkflowStepDef]) -> WorkflowStepDef {
    let used: Vec<&str> = existing.iter().map(|step| step.id.as_str()).collect();
    let mut id = name.to_owned();
    let mut n = 2;
    while used.contains(&id.as_str()) {
        id = format!("{name}-{n}");
        n += 1;
    }
    WorkflowStepDef {
        id,
        name: Some(name.to_owned()),
        prompt: Some("{{task}}".to_owned()),
        skill: Some(name.to_owned()),
        model: None,
        runner: None,
        allowed_tools: None,
        bash_allowlist: None,
        command: None,
        on_fail: None,
    }
}

pub fn open(app: &mut App, project: &str) {
    if app.workflows_ui.project != project {
        app.workflows_ui = WorkflowsUi {
            project: project.to_owned(),
            ..WorkflowsUi::default()
        };
    }
    app.request_navigate(Route::Workflows {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadWorkflows {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadWorkflowSkills {
        project: project.to_owned(),
    });
}

/// The step list the focused tab shows: the selected saved chain, or the draft.
fn current_steps(app: &App) -> &[WorkflowStepDef] {
    if app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len() {
        &app.workflows_ui.draft_steps
    } else {
        &app.workflows_ui.workflows[app.workflows_ui.selected_tab].steps
    }
}

fn current_steps_mut(app: &mut App) -> &mut Vec<WorkflowStepDef> {
    if app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len() {
        &mut app.workflows_ui.draft_steps
    } else {
        let index = app.workflows_ui.selected_tab;
        &mut app.workflows_ui.workflows[index].steps
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    render_tabs(frame, rows[0], app);

    if app.workflows_ui.workflows.is_empty() && app.workflows_ui.selected_tab == 0 {
        app.workflows_ui.selected_tab = app.workflows_ui.workflows.len();
    }

    let palette_width = (rows[1].width / 3).clamp(26, 40);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(palette_width), Constraint::Min(1)])
        .split(rows[1]);
    render_palette(frame, cols[0], app);
    render_steps(frame, cols[1], app);

    if app.workflows_ui.import_open {
        render_import_dialog(frame, area, app);
    }
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let mut spans = Vec::new();
    let mut x = area.x;
    for (index, workflow) in app.workflows_ui.workflows.iter().enumerate() {
        let active = index == app.workflows_ui.selected_tab;
        spans.push(Span::styled(
            format!(" {} ", workflow.name),
            if active {
                Style::default()
                    .fg(app.theme.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.palette.soft_fg)
            },
        ));
        app.hitmap.register(
            Rect::new(x, area.y, workflow.name.chars().count() as u16 + 2, 1),
            3,
            crate::input::hitmap::HitAction::WorkflowTab(index),
        );
        x = x.saturating_add(workflow.name.chars().count() as u16 + 2);
    }
    let new_active = app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len();
    let new_label = if new_active {
        " + new (draft) "
    } else {
        " + new "
    };
    spans.push(Span::styled(
        new_label.to_owned(),
        if new_active {
            Style::default()
                .fg(app.theme.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.palette.soft_fg)
        },
    ));
    app.hitmap.register(
        Rect::new(x, area.y, new_label.chars().count() as u16, 1),
        3,
        crate::input::hitmap::HitAction::WorkflowTab(app.workflows_ui.workflows.len()),
    );
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_steps(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = if app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len() {
        let name = if app.workflows_ui.draft_name.is_empty() {
            "untitled draft".to_owned()
        } else {
            app.workflows_ui.draft_name.clone()
        };
        format!("Steps — {name}")
    } else {
        format!(
            "Steps — {}",
            app.workflows_ui.workflows[app.workflows_ui.selected_tab].name
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if app.workflows_ui.focus == WorkflowFocus::Steps {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Owned copy: the hitmap registrations below need `&mut app` while the rows render.
    let steps: Vec<WorkflowStepDef> = current_steps(app).to_vec();
    if steps.is_empty() {
        frame.render_widget(
            Paragraph::new("No steps — pick skills from the palette (p), or press i to import.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            inner,
        );
    } else {
        let selected = app
            .workflows_ui
            .steps_selected
            .min(steps.len().saturating_sub(1));
        let lines: Vec<Line<'static>> = steps
            .iter()
            .enumerate()
            .take(inner.height as usize)
            .map(|(index, step)| step_line(index, step, &app.theme, index == selected))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        for (index, _) in steps.iter().enumerate().take(inner.height as usize) {
            if let Some(y) = inner.y.checked_add(index as u16)
                && y + 1 < inner.bottom()
            {
                app.hitmap.register(
                    Rect::new(inner.x, y, inner.width, 1),
                    2,
                    crate::input::hitmap::HitAction::WorkflowStep(index),
                );
            }
        }
        app.workflows_ui.steps_selected = selected;
    }

    // The action footer: save form hint + portable-form notice.
    let footer_y = inner.bottom().saturating_sub(1);
    let stack = skill_stack_of(current_steps(app));
    let footer = format!(
        "s save · i import · e export · x delete · Alt+j/k reorder · {}",
        match stack {
            Some(_) => "portable skills: form",
            None => "full steps: form",
        }
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            footer,
            Style::default().fg(app.theme.palette.soft_fg),
        ))),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

fn step_line(
    index: usize,
    step: &WorkflowStepDef,
    theme: &crate::theme::Theme,
    selected: bool,
) -> Line<'static> {
    let kind = if step.command.is_some() {
        "check"
    } else {
        "agent"
    };
    let label = step.name.clone().unwrap_or_else(|| step.id.clone());
    let detail = step
        .command
        .clone()
        .or_else(|| step.prompt.clone())
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(
            format!("{} ", index + 1),
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled(
            format!("[{kind}] "),
            Style::default().fg(if kind == "agent" {
                theme.palette.accent
            } else {
                theme.palette.waiting
            }),
        ),
        Span::styled(
            label.clone(),
            if selected {
                Style::default()
                    .fg(theme.palette.fg)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(theme.palette.fg)
            },
        ),
    ];
    if !detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", truncate(&detail, 60)),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    Line::from(spans)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut result: String = text.chars().take(max).collect();
        result.push('…');
        result
    }
}

fn render_palette(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Skills")
        .border_style(if app.workflows_ui.focus == WorkflowFocus::Palette {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![Line::from(Span::styled(
        format!("/ filter: {}", app.workflows_ui.palette_query),
        Style::default().fg(app.theme.palette.soft_fg),
    ))];
    let matches = palette_matches(app);
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "No skills match.",
            Style::default().fg(app.theme.palette.soft_fg),
        )));
    }
    for (position, index) in matches
        .iter()
        .enumerate()
        .take((inner.height as usize).saturating_sub(1))
    {
        let skill = &app.workflows_ui.palette_skills[*index];
        let selected = app.workflows_ui.focus == WorkflowFocus::Palette
            && *index == app.workflows_ui.palette_selected;
        let mut style = Style::default().fg(app.theme.palette.fg);
        if selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(skill.name.clone(), style)));
        if let Some(y) = inner.y.checked_add(1 + position as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                crate::input::hitmap::HitAction::WorkflowSkill(*index),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_import_dialog(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let width = area.width.min(80);
    let height = 14;
    let dialog = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let block = Block::default().borders(Borders::ALL).title("Import YAML");
    let inner = block.inner(dialog);
    frame.render_widget(Clear, dialog);
    frame.render_widget(block, dialog);
    let lines = app.workflows_ui.import_yaml.render_lines(
        "workflow.yaml",
        &app.workflows_ui.highlighter,
        &app.theme,
        inner.height.saturating_sub(1) as usize,
        true,
    );
    frame.render_widget(Paragraph::new(lines), inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Ctrl+Enter parses & applies · Esc cancels",
            Style::default().fg(app.theme.palette.soft_fg),
        ))),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.workflows_ui.import_open {
        return handle_import_key(app, key);
    }
    if let Some(name) = app.workflows_ui.delete_confirm.clone() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                app.workflows_ui.delete_confirm = None;
                let project = app.workflows_ui.project.clone();
                app.pending
                    .push(PendingAction::DeleteWorkflow { project, name });
            }
            KeyCode::Char('n') | KeyCode::Esc => app.workflows_ui.delete_confirm = None,
            _ => {}
        }
        return true;
    }
    if app.workflows_ui.name_open {
        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.workflows_ui.draft_name.push(character);
            }
            KeyCode::Backspace => {
                app.workflows_ui.draft_name.pop();
            }
            KeyCode::Enter | KeyCode::Esc => app.workflows_ui.name_open = false,
            _ => {}
        }
        return true;
    }
    if app.workflows_ui.focus == WorkflowFocus::Palette {
        return handle_palette_key(app, key);
    }
    match key.code {
        KeyCode::Tab | KeyCode::Char('l') => {
            let count = app.workflows_ui.workflows.len() + 1;
            app.workflows_ui.selected_tab = (app.workflows_ui.selected_tab + 1).min(count - 1);
            true
        }
        KeyCode::BackTab | KeyCode::Char('h') => {
            app.workflows_ui.selected_tab = app.workflows_ui.selected_tab.saturating_sub(1);
            true
        }
        KeyCode::Char('n') => {
            app.workflows_ui.selected_tab = app.workflows_ui.workflows.len();
            app.workflows_ui.draft_name.clear();
            app.workflows_ui.draft_steps.clear();
            app.workflows_ui.name_open = true;
            true
        }
        KeyCode::Char('N') => {
            if app.workflows_ui.selected_tab >= app.workflows_ui.workflows.len() {
                app.workflows_ui.name_open = !app.workflows_ui.name_open;
            }
            true
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::ALT) => {
            reorder(app, 1);
            true
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
            reorder(app, -1);
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let steps = current_steps(app);
            if !steps.is_empty() {
                app.workflows_ui.steps_selected =
                    (app.workflows_ui.steps_selected + 1).min(steps.len() - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.workflows_ui.steps_selected = app.workflows_ui.steps_selected.saturating_sub(1);
            true
        }
        KeyCode::Char('p') => {
            app.workflows_ui.focus = WorkflowFocus::Palette;
            app.workflows_ui.palette_selected = 0;
            true
        }
        KeyCode::Char('/') => {
            app.workflows_ui.focus = WorkflowFocus::Palette;
            app.workflows_ui.palette_query.clear();
            app.workflows_ui.palette_selected = 0;
            true
        }
        KeyCode::Char('s') => {
            let project = app.workflows_ui.project.clone();
            app.pending.push(PendingAction::SaveWorkflow { project });
            true
        }
        KeyCode::Char('e') => {
            let project = app.workflows_ui.project.clone();
            app.pending.push(PendingAction::ExportWorkflow { project });
            true
        }
        KeyCode::Char('i') => {
            app.workflows_ui.import_open = true;
            app.workflows_ui.import_yaml.set_text("");
            true
        }
        KeyCode::Char('x') => {
            if app.workflows_ui.selected_tab < app.workflows_ui.workflows.len() {
                let workflow = app.workflows_ui.workflows[app.workflows_ui.selected_tab].clone();
                if workflow.source == WorkflowSource::File {
                    app.workflows_ui.delete_confirm =
                        Some(format!("Delete workflow '{}'? [y/n]", workflow.name));
                } else {
                    app.notice = Some("built-in workflows cannot be deleted".to_owned());
                }
            } else {
                app.workflows_ui.draft_steps.pop();
            }
            true
        }
        KeyCode::Esc => {
            app.request_back();
            true
        }
        _ => false,
    }
}

/// Move the step the cursor points at by `delta` — the dnd-kit drag's keyboard twin
/// (`Alt+j`/`Alt+k`, the plan's "reorder by Alt+j/Alt+k").
fn reorder(app: &mut App, delta: i32) {
    let selected = app.workflows_ui.steps_selected;
    let steps = current_steps_mut(app);
    if steps.len() < 2 {
        return;
    }
    let index = selected.min(steps.len() - 1);
    let target = (index as i32 + delta).clamp(0, steps.len() as i32 - 1) as usize;
    if target != index {
        steps.swap(index, target);
        app.workflows_ui.steps_selected = target;
    }
}

fn handle_palette_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.workflows_ui.focus = WorkflowFocus::Steps;
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            move_palette_selection(app, 1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            move_palette_selection(app, -1);
            true
        }
        KeyCode::Enter => {
            let selected = app.workflows_ui.palette_selected;
            let Some(index) = palette_matches(app)
                .into_iter()
                .find(|index| *index == selected)
            else {
                return true;
            };
            let Some(skill) = app.workflows_ui.palette_skills.get(index).cloned() else {
                return true;
            };
            let steps = current_steps_mut(app);
            steps.push(skill_step(&skill.name, steps));
            app.workflows_ui.focus = WorkflowFocus::Steps;
            true
        }
        KeyCode::Backspace => {
            app.workflows_ui.palette_query.pop();
            app.workflows_ui.palette_selected = 0;
            true
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.workflows_ui.palette_query.push(character);
            app.workflows_ui.palette_selected = 0;
            true
        }
        _ => false,
    }
}

fn palette_matches(app: &App) -> Vec<usize> {
    let query = app.workflows_ui.palette_query.to_lowercase();
    app.workflows_ui
        .palette_skills
        .iter()
        .enumerate()
        .filter_map(|(index, skill)| {
            if query.is_empty()
                || skill.name.to_lowercase().contains(&query)
                || skill
                    .description
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
            {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

fn move_palette_selection(app: &mut App, delta: i32) {
    let matches = palette_matches(app);
    let Some(current) = matches
        .iter()
        .position(|index| *index == app.workflows_ui.palette_selected)
    else {
        if let Some(index) = matches.first() {
            app.workflows_ui.palette_selected = *index;
        }
        return;
    };
    let next = (current as i32 + delta).clamp(0, matches.len() as i32 - 1) as usize;
    app.workflows_ui.palette_selected = matches[next];
}

fn handle_import_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.workflows_ui.import_open = false;
            true
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let yaml = app.workflows_ui.import_yaml.text.clone();
            let project = app.workflows_ui.project.clone();
            app.workflows_ui.import_open = false;
            app.pending
                .push(PendingAction::ImportWorkflow { project, yaml });
            true
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.workflows_ui.import_yaml.insert_char(character);
            true
        }
        KeyCode::Enter => {
            app.workflows_ui.import_yaml.insert_newline();
            true
        }
        KeyCode::Backspace => {
            app.workflows_ui.import_yaml.backspace();
            true
        }
        KeyCode::Delete => {
            app.workflows_ui.import_yaml.delete_forward();
            true
        }
        _ => false,
    }
}

pub fn apply_tab_hit(app: &mut App, index: usize) {
    app.workflows_ui.selected_tab = index.min(app.workflows_ui.workflows.len());
    app.workflows_ui.steps_selected = 0;
}

pub fn apply_step_hit(app: &mut App, index: usize) {
    app.workflows_ui.steps_selected = index;
}

/// The mouse-drag drop: move the pressed step to where the pointer was released.
pub fn drop_step(app: &mut App, source: usize, target: usize) {
    let steps = current_steps_mut(app);
    if source >= steps.len() || target >= steps.len() || source == target {
        return;
    }
    let step = steps.remove(source);
    steps.insert(target.min(steps.len()), step);
    app.workflows_ui.steps_selected = target;
}

pub fn apply_palette_hit(app: &mut App, index: usize) {
    if index < app.workflows_ui.palette_skills.len() {
        app.workflows_ui.palette_selected = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app_with_workflows() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.workflows_ui.workflows = vec![WorkflowDef {
            name: "quick-task".to_owned(),
            description: None,
            steps: vec![skill_step("om-fix", &[])],
            source: WorkflowSource::BuiltIn,
            path: None,
        }];
        app.workflows_ui.palette_skills = vec![
            Skill {
                name: "om-fix".to_owned(),
                description: Some("Fix".to_owned()),
                interactive: None,
                body: "b".to_owned(),
                path: "p".to_owned(),
                source: coducktor_contract::SkillSource::Global,
            },
            Skill {
                name: "omarchy".to_owned(),
                description: Some("Desktop".to_owned()),
                interactive: None,
                body: "b".to_owned(),
                path: "p".to_owned(),
                source: coducktor_contract::SkillSource::Global,
            },
        ];
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
    fn skill_stack_of_accepts_only_plain_skill_steps() {
        let plain = vec![
            skill_step("om-fix", &[]),
            skill_step("omarchy", &[skill_step("om-fix", &[])]),
        ];
        assert_eq!(
            skill_stack_of(&plain),
            Some(vec!["om-fix".to_owned(), "omarchy".to_owned()])
        );

        let mut custom_prompt = skill_step("om-fix", &[]);
        custom_prompt.prompt = Some("do it differently".to_owned());
        assert_eq!(skill_stack_of(&[custom_prompt]), None);

        let mut named = skill_step("om-fix", &[]);
        named.name = Some("Fancy name".to_owned());
        assert_eq!(skill_stack_of(&[named]), None);

        let mut check = skill_step("om-fix", &[]);
        check.command = Some("cargo test".to_owned());
        assert_eq!(skill_stack_of(&[check]), None);

        assert_eq!(skill_stack_of(&[]), None);
    }

    #[test]
    fn palette_enter_appends_a_deduped_step_and_the_draft_renders() {
        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1; // + new
        app.workflows_ui.draft_name = "my-chain".to_owned();
        app.workflows_ui.focus = WorkflowFocus::Palette;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.workflows_ui.draft_steps.len(), 1);
        assert_eq!(
            app.workflows_ui.draft_steps[0].skill.as_deref(),
            Some("om-fix")
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.workflows_ui.draft_steps.len(), 2);
        assert_eq!(app.workflows_ui.draft_steps[1].id, "om-fix-2");
        let content = render(&mut app, 120, 40);
        assert!(content.contains("my-chain"));
        assert!(content.contains("agent"));
    }

    #[test]
    fn palette_arrows_move_between_visible_skills() {
        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1;
        app.workflows_ui.focus = WorkflowFocus::Palette;

        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.workflows_ui.palette_selected, 1);
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.workflows_ui.palette_selected, 0);

        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.workflows_ui.draft_steps.len(), 1);
        assert_eq!(
            app.workflows_ui.draft_steps[0].skill.as_deref(),
            Some("omarchy")
        );
    }

    #[test]
    fn alt_j_alt_k_reorder_the_cursor_step() {
        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1;
        app.workflows_ui.draft_steps = vec![
            skill_step("om-fix", &[]),
            skill_step("omarchy", &[skill_step("om-fix", &[])]),
        ];
        // Cursor down to omarchy, Alt+k moves it up.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        let mut key = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT);
        handle_key(&mut app, key);
        assert_eq!(
            app.workflows_ui.draft_steps[0].skill.as_deref(),
            Some("omarchy")
        );
        assert_eq!(app.workflows_ui.steps_selected, 0);
        // Alt+j moves it back down.
        key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        handle_key(&mut app, key);
        assert_eq!(
            app.workflows_ui.draft_steps[0].skill.as_deref(),
            Some("om-fix")
        );
        assert_eq!(app.workflows_ui.steps_selected, 1);
        // At the bottom, Alt+j is a no-op.
        handle_key(&mut app, key);
        assert_eq!(
            app.workflows_ui.draft_steps[0].skill.as_deref(),
            Some("om-fix")
        );
    }

    #[test]
    fn save_pushes_the_compact_skills_form_when_the_stack_is_portable() {
        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1;
        app.workflows_ui.draft_name = "portable".to_owned();
        app.workflows_ui.draft_steps = vec![skill_step("om-fix", &[])];
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );
        let save = app
            .pending
            .iter()
            .find(|action| matches!(action, PendingAction::SaveWorkflow { .. }))
            .expect("save queued");
        assert!(matches!(save, PendingAction::SaveWorkflow { .. }));
    }

    #[test]
    fn deleting_a_built_in_workflow_is_refused() {
        let mut app = app_with_workflows();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(app.workflows_ui.delete_confirm.is_none());
        assert_eq!(
            app.notice.as_deref(),
            Some("built-in workflows cannot be deleted")
        );
    }

    #[test]
    fn mouse_press_drag_release_reorders_steps() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1;
        app.workflows_ui.draft_steps = vec![
            skill_step("om-fix", &[]),
            skill_step("omarchy", &[skill_step("om-fix", &[])]),
        ];
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        // Locate the two step rows through the hitmap itself — no hardcoded geometry.
        let mut rows: Vec<(usize, u16)> = Vec::new();
        'scan: for y in 0..24u16 {
            for x in 0..120u16 {
                if let Some(crate::input::hitmap::HitAction::WorkflowStep(index)) =
                    app.hitmap.hit(x, y)
                    && !rows.iter().any(|(_, row)| *row == y)
                {
                    rows.push((index, y));
                    if rows.len() == 2 {
                        break 'scan;
                    }
                }
            }
        }
        assert_eq!(rows.len(), 2, "two step rows are clickable");
        let (source_index, source_row) = rows[0];
        let (target_index, target_row) = rows[1];
        let mouse = |kind| MouseEvent {
            kind,
            column: 100,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        // Press the first row (arms the drag + selects), release on the second (drops).
        app.handle_event(crossterm::event::Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 100,
            row: source_row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.workflows_ui.drag_source, Some(source_index));
        app.handle_event(crossterm::event::Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 100,
            row: target_row,
            modifiers: KeyModifiers::NONE,
        }));
        let _ = mouse(MouseEventKind::Up(MouseButton::Left));
        assert!(
            app.workflows_ui.drag_source.is_none(),
            "drag cleared on release"
        );
        assert_eq!(
            app.workflows_ui
                .draft_steps
                .iter()
                .map(|step| step.skill.clone().unwrap())
                .collect::<Vec<_>>(),
            vec!["omarchy".to_owned(), "om-fix".to_owned()]
        );
        assert_eq!(app.workflows_ui.steps_selected, target_index);
    }

    #[test]
    fn snapshot_workflows_at_three_sizes() {
        let mut app = app_with_workflows();
        app.workflows_ui.selected_tab = 1;
        app.workflows_ui.draft_name = "my-chain".to_owned();
        app.workflows_ui.draft_steps = vec![
            skill_step("om-fix", &[]),
            skill_step("omarchy", &[skill_step("om-fix", &[])]),
        ];
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("workflows_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
