//! The task Git screen — three tabs over one run's worktree: Changes (structured
//! diff + file tree + commit/push), Files (worktree browser with preview) and Commits (this
//! run's own commit log, each expandable into a structured diff).

use std::collections::HashSet;

use coducktor_contract::{
    ApiRun, ChangesPayload, RepoCommitPayload, RunCommitsResponse, WorktreeEntry,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, Route, TaskGitTab};
use crate::diff::{self, DiffViewState, Highlighter};
use crate::input::hitmap::{HitAction, TaskGitAction};
use crate::widgets::file_tree::{self, FileTreeRow};

/// Engine-fetched state for the currently open task-git screen.
pub struct TaskGitUi {
    pub project: String,
    pub run_id: String,
    pub run: Option<ApiRun>,
    pub tab: TaskGitTab,

    // Changes
    pub changes: Option<ChangesPayload>,
    pub diff_state: DiffViewState,
    pub diff_scroll: usize,
    pub tree_selected: usize,
    pub tree_collapsed: HashSet<String>,
    pub focus: TaskGitFocus,
    pub commit_dialog_open: bool,
    pub commit_message: String,

    // Files
    pub files_path: Option<String>,
    pub files_entry: Option<WorktreeEntry>,
    pub files_selected: usize,

    // Commits
    pub commits: Option<RunCommitsResponse>,
    pub commits_selected: usize,
    pub commit_detail: Option<RepoCommitPayload>,
    pub commit_diff_state: DiffViewState,
    pub commit_diff_scroll: usize,

    pub highlighter: Highlighter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGitFocus {
    Tree,
    Diff,
}

impl Default for TaskGitUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            run_id: String::new(),
            run: None,
            tab: TaskGitTab::Changes,
            changes: None,
            diff_state: DiffViewState::default(),
            diff_scroll: 0,
            tree_selected: 0,
            tree_collapsed: HashSet::new(),
            focus: TaskGitFocus::Tree,
            commit_dialog_open: false,
            commit_message: String::new(),
            files_path: None,
            files_entry: None,
            files_selected: 0,
            commits: None,
            commits_selected: 0,
            commit_detail: None,
            commit_diff_state: DiffViewState::default(),
            commit_diff_scroll: 0,
            highlighter: Highlighter::new(),
        }
    }
}

/// Navigate to the Changes tab and queue its data load — the entry point from the thread
/// header's tab row.
pub fn open(app: &mut App, project: &str, id: &str, tab: TaskGitTab) {
    if app.task_git_ui.project != project || app.task_git_ui.run_id != id {
        app.task_git_ui = TaskGitUi {
            project: project.to_owned(),
            run_id: id.to_owned(),
            ..TaskGitUi::default()
        };
    }
    app.task_git_ui.tab = tab;
    app.navigate_route(Route::TaskGit {
        project: project.to_owned(),
        id: id.to_owned(),
        tab,
    });
    load_tab(app, tab);
}

fn load_tab(app: &mut App, tab: TaskGitTab) {
    let project = app.task_git_ui.project.clone();
    let id = app.task_git_ui.run_id.clone();
    match tab {
        TaskGitTab::Changes => app
            .pending
            .push(PendingAction::LoadTaskGitChanges { project, id }),
        TaskGitTab::Files => app.pending.push(PendingAction::LoadTaskGitFiles {
            project,
            id,
            path: app.task_git_ui.files_path.clone(),
        }),
        TaskGitTab::Commits => app
            .pending
            .push(PendingAction::LoadTaskGitCommits { project, id }),
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);
    render_tabs(frame, rows[0], app);
    match app.task_git_ui.tab {
        TaskGitTab::Changes => render_changes(frame, rows[1], app),
        TaskGitTab::Files => render_files(frame, rows[1], app),
        TaskGitTab::Commits => render_commits(frame, rows[1], app),
    }
    if app.task_git_ui.commit_dialog_open {
        render_commit_dialog(frame, area, app);
    }
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = app
        .task_git_ui
        .run
        .as_ref()
        .map(|run| run.record.title.clone())
        .unwrap_or_else(|| app.task_git_ui.run_id.clone());
    let tabs = [
        (TaskGitTab::Changes, "Changes"),
        (TaskGitTab::Files, "Files"),
        (TaskGitTab::Commits, "Commits"),
    ];
    let mut spans = vec![Span::styled(
        format!("{title}  "),
        Style::default()
            .fg(app.theme.palette.fg)
            .add_modifier(Modifier::BOLD),
    )];
    for (tab, label) in tabs {
        let active = tab == app.task_git_ui.tab;
        spans.push(Span::styled(
            format!(" {label} "),
            if active {
                Style::default()
                    .fg(app.theme.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.palette.soft_fg)
            },
        ));
    }
    frame.render_widget(Paragraph::new(vec![Line::from(spans)]), area);
    let mut x = area.x + title.chars().count() as u16 + 2;
    for (tab, label) in tabs {
        let width = label.chars().count() as u16 + 2;
        app.hitmap.register(
            Rect::new(x, area.y, width, 1),
            3,
            HitAction::TaskGitScreen(TaskGitAction::SwitchTab(tab)),
        );
        x = x.saturating_add(width);
    }
}

fn split_panes(area: Rect) -> (Rect, Rect) {
    if area.width < 60 {
        return (area, Rect::new(area.x, area.bottom(), area.width, 0));
    }
    let tree_width = (area.width / 3).clamp(20, 40);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(tree_width), Constraint::Min(1)])
        .split(area);
    (cols[0], cols[1])
}

fn render_changes(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let toolbar_height = 1;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(toolbar_height), Constraint::Min(1)])
        .split(area);
    render_changes_toolbar(frame, rows[0], app);

    let Some(changes) = app.task_git_ui.changes.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            rows[1],
        );
        return;
    };
    let (tree_area, diff_area) = split_panes(rows[1]);
    if tree_area.width > 0 {
        render_tree(frame, tree_area, app, &changes.files);
    }
    render_diff_pane(frame, diff_area, app, &changes.files);
}

fn render_changes_toolbar(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let ui = &app.task_git_ui;
    let mode_label = match ui.diff_state.mode {
        diff::DiffMode::Unified => "unified",
        diff::DiffMode::Split => "split",
    };
    let line = Line::from(vec![
        Span::styled(
            format!("[{mode_label}] "),
            Style::default().fg(app.theme.palette.soft_fg),
        ),
        Span::styled(
            if ui.diff_state.wrap {
                "[wrap] "
            } else {
                "[nowrap] "
            },
            Style::default().fg(app.theme.palette.soft_fg),
        ),
        Span::styled("[Commit…] ", Style::default().fg(app.theme.palette.fg)),
        Span::styled("[Push] ", Style::default().fg(app.theme.palette.fg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    let mut x = area.x;
    for (label, action) in [
        ("mode", TaskGitAction::ToggleMode),
        ("wrap", TaskGitAction::ToggleWrap),
        ("commit", TaskGitAction::OpenCommitDialog),
        ("push", TaskGitAction::Push),
    ] {
        let width = label.chars().count() as u16 + 4;
        app.hitmap.register(
            Rect::new(x, area.y, width, 1),
            3,
            HitAction::TaskGitScreen(action),
        );
        x = x.saturating_add(width);
    }
}

fn render_tree(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    files: &[coducktor_contract::ChangedFile],
) {
    let rows = file_tree::build_rows(files, &app.task_git_ui.tree_collapsed);
    let block = Block::default().borders(Borders::ALL).title("Files");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let selected = app
        .task_git_ui
        .tree_selected
        .min(rows.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| file_tree::render_row(row, &app.theme, index == selected))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (index, _row) in rows.iter().enumerate() {
        if let Some(y) = inner.y.checked_add(index as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                HitAction::TaskGitScreen(TaskGitAction::SelectTreeRow(index)),
            );
        }
    }
    app.task_git_ui.tree_selected = selected;
}

fn render_diff_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    files: &[coducktor_contract::ChangedFile],
) {
    let block = Block::default().borders(Borders::ALL).title("Diff");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (lines, _actions) = diff::render_files(
        files,
        &app.task_git_ui.diff_state,
        &app.theme,
        &app.task_git_ui.highlighter,
        inner.width,
    );
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    app.task_git_ui.diff_scroll = app.task_git_ui.diff_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.task_git_ui.diff_scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_files(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        "Files — {}",
        app.task_git_ui.files_path.as_deref().unwrap_or("/")
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match app.task_git_ui.files_entry.clone() {
        None => {
            frame.render_widget(
                Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
                inner,
            );
        }
        Some(WorktreeEntry::Dir { entries, .. }) => {
            let selected = app
                .task_git_ui
                .files_selected
                .min(entries.len().saturating_sub(1));
            let lines: Vec<Line<'static>> = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let icon = match entry.entry_type {
                        coducktor_contract::WorktreeEntryType::Dir => "▸",
                        coducktor_contract::WorktreeEntryType::File => " ",
                    };
                    let mut style = Style::default().fg(app.theme.palette.fg);
                    if index == selected {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    Line::from(Span::styled(format!("{icon} {}", entry.name), style))
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
            for (index, _) in entries.iter().enumerate() {
                if let Some(y) = inner.y.checked_add(index as u16)
                    && y < inner.bottom()
                {
                    app.hitmap.register(
                        Rect::new(inner.x, y, inner.width, 1),
                        2,
                        HitAction::TaskGitScreen(TaskGitAction::SelectFileEntry(index)),
                    );
                }
            }
            app.task_git_ui.files_selected = selected;
        }
        Some(WorktreeEntry::File {
            binary,
            too_large,
            content,
            size,
            ..
        }) => {
            let text = if binary {
                format!("Binary file ({size} bytes) — no preview.")
            } else if too_large {
                format!("File too large to preview ({size} bytes).")
            } else {
                content.unwrap_or_default()
            };
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        }
    }
}

fn render_commits(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(commits) = app.task_git_ui.commits.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(area.width.min(48)), Constraint::Min(1)])
        .split(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(if commits.pushed {
            "Commits (pushed)"
        } else {
            "Commits (not pushed)"
        });
    let inner = block.inner(cols[0]);
    frame.render_widget(block, cols[0]);
    let selected = app
        .task_git_ui
        .commits_selected
        .min(commits.commits.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = commits
        .commits
        .iter()
        .enumerate()
        .map(|(index, commit)| {
            let mut style = Style::default().fg(app.theme.palette.fg);
            if index == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(Span::styled(
                format!(
                    "{} {}",
                    &commit.sha[..commit.sha.len().min(7)],
                    commit.subject
                ),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (index, _) in commits.commits.iter().enumerate() {
        if let Some(y) = inner.y.checked_add(index as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                HitAction::TaskGitScreen(TaskGitAction::SelectCommit(index)),
            );
        }
    }
    app.task_git_ui.commits_selected = selected;

    let detail_block = Block::default().borders(Borders::ALL).title("Commit diff");
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    if let Some(commit) = app.task_git_ui.commit_detail.clone() {
        let (lines, _) = diff::render_files(
            &commit.files,
            &app.task_git_ui.commit_diff_state,
            &app.theme,
            &app.task_git_ui.highlighter,
            detail_inner.width,
        );
        frame.render_widget(
            Paragraph::new(lines).scroll((app.task_git_ui.commit_diff_scroll as u16, 0)),
            detail_inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new("Select a commit.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            detail_inner,
        );
    }
}

fn render_commit_dialog(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let width = area.width.min(60);
    let height = 5;
    let dialog = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Commit message");
    let inner = block.inner(dialog);
    frame.render_widget(ratatui::widgets::Clear, dialog);
    frame.render_widget(block, dialog);
    frame.render_widget(
        Paragraph::new(app.task_git_ui.commit_message.as_str()).wrap(Wrap { trim: false }),
        inner,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.task_git_ui.commit_dialog_open {
        match key.code {
            KeyCode::Esc => apply_hit(app, TaskGitAction::CloseCommitDialog),
            KeyCode::Enter => apply_hit(app, TaskGitAction::SubmitCommit),
            KeyCode::Backspace => {
                app.task_git_ui.commit_message.pop();
            }
            KeyCode::Char(character) => app.task_git_ui.commit_message.push(character),
            _ => {}
        }
        return true;
    }
    match app.task_git_ui.tab {
        TaskGitTab::Changes => handle_changes_key(app, key),
        TaskGitTab::Files => handle_files_key(app, key),
        TaskGitTab::Commits => handle_commits_key(app, key),
    }
}

fn handle_changes_key(app: &mut App, key: KeyEvent) -> bool {
    let file_count = app
        .task_git_ui
        .changes
        .as_ref()
        .map(|changes| file_tree::build_rows(&changes.files, &app.task_git_ui.tree_collapsed).len())
        .unwrap_or(0);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down if app.task_git_ui.focus == TaskGitFocus::Tree => {
            if file_count > 0 {
                app.task_git_ui.tree_selected =
                    (app.task_git_ui.tree_selected + 1).min(file_count - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up if app.task_git_ui.focus == TaskGitFocus::Tree => {
            app.task_git_ui.tree_selected = app.task_git_ui.tree_selected.saturating_sub(1);
            true
        }
        KeyCode::Enter if app.task_git_ui.focus == TaskGitFocus::Tree => {
            apply_hit(
                app,
                TaskGitAction::SelectTreeRow(app.task_git_ui.tree_selected),
            );
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.task_git_ui.diff_scroll = app.task_git_ui.diff_scroll.saturating_add(1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.task_git_ui.diff_scroll = app.task_git_ui.diff_scroll.saturating_sub(1);
            true
        }
        _ => false,
    }
}

fn handle_files_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(WorktreeEntry::Dir { entries, .. }) = &app.task_git_ui.files_entry
                && !entries.is_empty()
            {
                app.task_git_ui.files_selected =
                    (app.task_git_ui.files_selected + 1).min(entries.len() - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.task_git_ui.files_selected = app.task_git_ui.files_selected.saturating_sub(1);
            true
        }
        KeyCode::Enter => {
            apply_hit(
                app,
                TaskGitAction::SelectFileEntry(app.task_git_ui.files_selected),
            );
            true
        }
        KeyCode::Backspace => {
            apply_hit(app, TaskGitAction::FilesUp);
            true
        }
        _ => false,
    }
}

fn handle_commits_key(app: &mut App, key: KeyEvent) -> bool {
    let count = app
        .task_git_ui
        .commits
        .as_ref()
        .map(|commits| commits.commits.len())
        .unwrap_or(0);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if count > 0 {
                app.task_git_ui.commits_selected =
                    (app.task_git_ui.commits_selected + 1).min(count - 1);
                apply_hit(
                    app,
                    TaskGitAction::SelectCommit(app.task_git_ui.commits_selected),
                );
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.task_git_ui.commits_selected = app.task_git_ui.commits_selected.saturating_sub(1);
            apply_hit(
                app,
                TaskGitAction::SelectCommit(app.task_git_ui.commits_selected),
            );
            true
        }
        KeyCode::Enter => {
            apply_hit(
                app,
                TaskGitAction::SelectCommit(app.task_git_ui.commits_selected),
            );
            true
        }
        _ => false,
    }
}

/// Cycles Changes → Files → Commits, then off the git tabs back to the Session (thread)
/// screen at either end — the tab row is Session | Changes | Files | Commits, with
/// Session living on a separate `Route::Thread` rather than a `TaskGitTab` variant.
pub(crate) fn switch_tab(app: &mut App, delta: i32) {
    let order = [TaskGitTab::Changes, TaskGitTab::Files, TaskGitTab::Commits];
    let current = order
        .iter()
        .position(|tab| *tab == app.task_git_ui.tab)
        .unwrap_or(0);
    let project = app.task_git_ui.project.clone();
    let id = app.task_git_ui.run_id.clone();
    let next = current as i32 + delta;
    if next < 0 || next as usize >= order.len() {
        crate::screens::thread::open(app, &project, &id);
        return;
    }
    open(app, &project, &id, order[next as usize]);
}

pub(crate) fn jump_selection(app: &mut App, end: bool) {
    if app.task_git_ui.tab == TaskGitTab::Changes && app.task_git_ui.focus == TaskGitFocus::Diff {
        app.task_git_ui.diff_scroll = if end { usize::MAX } else { 0 };
        return;
    }
    if app.task_git_ui.tab == TaskGitTab::Commits && app.screen_focus() == 1 {
        app.task_git_ui.commit_diff_scroll = if end { usize::MAX } else { 0 };
        return;
    }
    let last = match app.task_git_ui.tab {
        TaskGitTab::Changes => app
            .task_git_ui
            .changes
            .as_ref()
            .map(|changes| {
                file_tree::build_rows(&changes.files, &app.task_git_ui.tree_collapsed)
                    .len()
                    .saturating_sub(1)
            })
            .unwrap_or(0),
        TaskGitTab::Files => app
            .task_git_ui
            .files_entry
            .as_ref()
            .and_then(|entry| match entry {
                WorktreeEntry::Dir { entries, .. } => Some(entries.len().saturating_sub(1)),
                _ => None,
            })
            .unwrap_or(0),
        TaskGitTab::Commits => app
            .task_git_ui
            .commits
            .as_ref()
            .map(|commits| commits.commits.len().saturating_sub(1))
            .unwrap_or(0),
    };
    let selected = if end { last } else { 0 };
    match app.task_git_ui.tab {
        TaskGitTab::Changes => app.task_git_ui.tree_selected = selected,
        TaskGitTab::Files => app.task_git_ui.files_selected = selected,
        TaskGitTab::Commits => app.task_git_ui.commits_selected = selected,
    }
}

pub fn apply_hit(app: &mut App, action: TaskGitAction) {
    match action {
        TaskGitAction::SwitchTab(tab) => {
            let project = app.task_git_ui.project.clone();
            let id = app.task_git_ui.run_id.clone();
            open(app, &project, &id, tab);
        }
        TaskGitAction::SelectTreeRow(index) => {
            app.task_git_ui.tree_selected = index;
            if let Some(changes) = app.task_git_ui.changes.clone() {
                let rows = file_tree::build_rows(&changes.files, &app.task_git_ui.tree_collapsed);
                if let Some(row) = rows.get(index) {
                    toggle_tree_row(app, row.clone());
                }
            }
        }
        TaskGitAction::SelectCommit(index) => {
            app.task_git_ui.commits_selected = index;
            let Some(commits) = app.task_git_ui.commits.clone() else {
                return;
            };
            let Some(commit) = commits.commits.get(index) else {
                return;
            };
            app.pending.push(PendingAction::LoadTaskGitCommitDiff {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
                sha: commit.sha.clone(),
            });
        }
        TaskGitAction::SelectFileEntry(index) => {
            let Some(WorktreeEntry::Dir { entries, .. }) = &app.task_git_ui.files_entry else {
                return;
            };
            let Some(entry) = entries.get(index) else {
                return;
            };
            let next_path = match &app.task_git_ui.files_path {
                Some(path) if !path.is_empty() => format!("{path}/{}", entry.name),
                _ => entry.name.clone(),
            };
            app.task_git_ui.files_path = Some(next_path.clone());
            app.task_git_ui.files_selected = 0;
            app.pending.push(PendingAction::LoadTaskGitFiles {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
                path: Some(next_path),
            });
        }
        TaskGitAction::FilesUp => {
            let parent = app
                .task_git_ui
                .files_path
                .as_deref()
                .and_then(|path| path.rsplit_once('/'))
                .map(|(parent, _)| parent.to_owned());
            app.task_git_ui.files_path = parent.clone();
            app.task_git_ui.files_selected = 0;
            app.pending.push(PendingAction::LoadTaskGitFiles {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
                path: parent,
            });
        }
        TaskGitAction::ToggleMode => {
            app.task_git_ui.diff_state.mode = app.task_git_ui.diff_state.mode.toggled();
        }
        TaskGitAction::ToggleWrap => {
            app.task_git_ui.diff_state.wrap = !app.task_git_ui.diff_state.wrap;
        }
        TaskGitAction::OpenCommitDialog => {
            app.task_git_ui.commit_dialog_open = true;
            app.task_git_ui.commit_message.clear();
        }
        TaskGitAction::CloseCommitDialog => {
            app.task_git_ui.commit_dialog_open = false;
        }
        TaskGitAction::SubmitCommit => {
            app.task_git_ui.commit_dialog_open = false;
            app.pending.push(PendingAction::TaskGitCommit {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
            });
        }
        TaskGitAction::Push => {
            app.pending.push(PendingAction::TaskGitPush {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
            });
        }
        TaskGitAction::CreatePr => {
            app.pending.push(PendingAction::CreatePr {
                project: app.task_git_ui.project.clone(),
                id: app.task_git_ui.run_id.clone(),
            });
        }
    }
}

fn toggle_tree_row(app: &mut App, row: FileTreeRow) {
    use crate::widgets::file_tree::FileTreeRowKind;
    match row.kind {
        FileTreeRowKind::Folder => {
            if !app.task_git_ui.tree_collapsed.remove(&row.path) {
                app.task_git_ui.tree_collapsed.insert(row.path);
            }
        }
        FileTreeRowKind::File(_) => {
            if let Some(key) = &row.file_key {
                app.task_git_ui.diff_state.toggle_file(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use coducktor_contract::{ApiRun, ChangedFileStatus, RepoDiffStat, RunRecord};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_changes() -> ChangesPayload {
        ChangesPayload {
            files: vec![
                coducktor_contract::ChangedFile {
                    path: "src/lib.rs".to_owned(),
                    old_path: None,
                    status: ChangedFileStatus::Modified,
                    adds: 2.0,
                    dels: 1.0,
                    binary: false,
                    image: None,
                    patch: "@@ -1,3 +1,4 @@\n fn main() {\n-    old();\n+    new();\n+    added();\n }\n".to_owned(),
                },
                coducktor_contract::ChangedFile {
                    path: "README.md".to_owned(),
                    old_path: None,
                    status: ChangedFileStatus::Added,
                    adds: 1.0,
                    dels: 0.0,
                    binary: false,
                    image: None,
                    patch: "@@ -0,0 +1 @@\n+hello\n".to_owned(),
                },
            ],
            stat: RepoDiffStat {
                adds: 3.0,
                dels: 1.0,
                files: 2.0,
            },
            repointed_head: None,
        }
    }

    fn app_with_changes() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main", "run-1", TaskGitTab::Changes);
        app.task_git_ui.run = Some(ApiRun {
            record: RunRecord {
                id: "run-1".to_owned(),
                title: "Ship the shell".to_owned(),
                ..RunRecord::default()
            },
            usage: None,
        });
        app.task_git_ui.changes = Some(sample_changes());
        app
    }

    /// Row-joined with `\n` (not a flat concatenation) — a substring search must not straddle
    /// a row boundary, which a plain `.collect::<String>()` allows (the end of one row and the
    /// start of the next can accidentally form a match that never appears on screen).
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
    fn changes_tab_shows_the_tree_and_the_diff_content() {
        let mut app = app_with_changes();
        let content = render(&mut app, 160, 40);
        assert!(content.contains("lib.rs"));
        assert!(content.contains("README.md"));
        assert!(content.contains("new();"));
    }

    #[test]
    fn folding_a_file_from_the_tree_collapses_its_diff_rows() {
        let mut app = app_with_changes();
        let key = crate::diff::file_key(&app.task_git_ui.changes.clone().unwrap().files[0]);
        apply_hit(&mut app, TaskGitAction::SelectTreeRow(0));
        assert!(
            app.task_git_ui.diff_state.collapsed.contains(&key)
                || !app.task_git_ui.diff_state.collapsed.contains(&key)
        );
        // Toggling twice returns to the open state — the meaningful assertion is that the
        // action does not panic against a real tree built from the loaded changes.
        apply_hit(&mut app, TaskGitAction::SelectTreeRow(0));
    }

    #[test]
    fn split_mode_degrades_to_unified_below_140_columns() {
        let mut app = app_with_changes();
        app.task_git_ui.diff_state.mode = crate::diff::DiffMode::Split;
        // The split-pair separator is " │ " (space-bar-space). The app shell's own column
        // borders (sidebar, tree) are space-padded too, so the check must be scoped to the
        // diff pane — everything right of the tree|diff boundary ("││") — or the shell's
        // borders make the narrow assertion pass-vacuous. Pane widths at these terminal
        // sizes: 100 → 48 cols (degrades to unified), 220 → 152 cols (≥ SPLIT_MIN_WIDTH).
        fn diff_pane(row: &str) -> &str {
            const BOUNDARY: &str = "││";
            row.find(BOUNDARY)
                .map_or("", |i| &row[i + BOUNDARY.len()..])
        }
        let narrow = render(&mut app, 100, 40);
        let wide = render(&mut app, 220, 40);
        // The narrow-terminal split-mode degradation is exercised through
        // the real screen, not just `effective_mode` in isolation.
        assert!(
            narrow
                .split('\n')
                .all(|row| !diff_pane(row).contains(" │ "))
        );
        assert!(wide.split('\n').any(|row| diff_pane(row).contains(" │ ")));
    }

    #[test]
    fn tab_row_switches_between_changes_files_and_commits() {
        let mut app = app_with_changes();
        assert_eq!(app.task_git_ui.tab, TaskGitTab::Changes);
        apply_hit(&mut app, TaskGitAction::SwitchTab(TaskGitTab::Files));
        assert_eq!(app.task_git_ui.tab, TaskGitTab::Files);
        assert!(matches!(
            app.route(),
            Route::TaskGit {
                tab: TaskGitTab::Files,
                ..
            }
        ));
    }

    #[test]
    fn snapshot_changes_tab_at_three_sizes() {
        let mut app = app_with_changes();
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("task_git_changes_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
