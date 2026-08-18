//! The repo Git screen — Commits, Changes, and Branches over the main working tree.

use coducktor_contract::{LogEntry, RepoCommitPayload, RepoResponse};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, RepoGitTab, Route};
use crate::diff::{self, DiffViewState, Highlighter};
use crate::input::hitmap::{HitAction, RepoGitAction};
use crate::widgets::file_tree;

pub struct RepoGitUi {
    pub project: String,
    pub tab: RepoGitTab,
    pub repo: Option<RepoResponse>,
    /// The main working tree's structured diff, separate from repository status, log, and branch
    /// metadata.
    pub repo_changes_files: Vec<coducktor_contract::ChangedFile>,
    pub changes_loading: bool,
    pub diff_state: DiffViewState,
    pub diff_scroll: usize,
    pub tree_selected: usize,
    pub tree_collapsed: std::collections::HashSet<String>,

    pub commits_selected: usize,
    pub commit_detail: Option<RepoCommitPayload>,
    pub commit_diff_state: DiffViewState,
    pub commit_diff_scroll: usize,

    pub branches_selected: usize,
    pub new_branch_name: String,
    pub new_branch_open: bool,

    pub highlighter: Highlighter,
}

impl Default for RepoGitUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            tab: RepoGitTab::Commits,
            repo: None,
            repo_changes_files: Vec::new(),
            changes_loading: false,
            diff_state: DiffViewState::default(),
            diff_scroll: 0,
            tree_selected: 0,
            tree_collapsed: std::collections::HashSet::new(),
            commits_selected: 0,
            commit_detail: None,
            commit_diff_state: DiffViewState::default(),
            commit_diff_scroll: 0,
            branches_selected: 0,
            new_branch_name: String::new(),
            new_branch_open: false,
            highlighter: Highlighter::new(true),
        }
    }
}

pub fn open(app: &mut App, project: &str, tab: RepoGitTab) {
    if app.repo_git_ui.project != project {
        app.repo_git_ui = RepoGitUi {
            project: project.to_owned(),
            ..RepoGitUi::default()
        };
    }
    app.repo_git_ui.tab = tab;
    app.navigate_route(Route::RepoGit {
        project: project.to_owned(),
        tab,
    });
    app.repo_git_ui.changes_loading = matches!(tab, RepoGitTab::Changes | RepoGitTab::Branches);
    match tab {
        RepoGitTab::Changes | RepoGitTab::Branches => {
            app.pending.push(PendingAction::LoadRepoGit {
                project: project.to_owned(),
            });
        }
        RepoGitTab::Commits => {
            app.pending.push(PendingAction::LoadRepoGitCommits {
                project: project.to_owned(),
            });
        }
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    render_tabs(frame, rows[0], app);
    let Some(repo) = app.repo_git_ui.repo.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            rows[1],
        );
        return;
    };
    let RepoResponse::Present(present) = repo else {
        frame.render_widget(
            Paragraph::new("Not a git repository.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            rows[1],
        );
        return;
    };
    match app.repo_git_ui.tab {
        RepoGitTab::Changes => render_changes(frame, rows[1], app),
        RepoGitTab::Commits => render_commits(frame, rows[1], app, &present.log),
        RepoGitTab::Branches => render_branches(
            frame,
            rows[1],
            app,
            &present.branches,
            present.base_branch.as_deref(),
        ),
    }
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let tabs = [
        (RepoGitTab::Commits, "Commits"),
        (RepoGitTab::Changes, "Changes"),
        (RepoGitTab::Branches, "Branches"),
    ];
    let mut spans = Vec::new();
    for (tab, label) in tabs {
        let active = tab == app.repo_git_ui.tab;
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
    let mut x = area.x;
    for (tab, label) in tabs {
        let width = label.chars().count() as u16 + 2;
        app.hitmap.register(
            Rect::new(x, area.y, width, 1),
            3,
            HitAction::RepoGitScreen(RepoGitAction::SwitchTab(tab)),
        );
        x = x.saturating_add(width);
    }
}

fn render_changes(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(RepoResponse::Present(_)) = &app.repo_git_ui.repo else {
        return;
    };
    let files: Vec<coducktor_contract::ChangedFile> = app.repo_git_ui.changes_files();
    let cols = if area.width < 60 {
        vec![area]
    } else {
        let tree_width = (area.width / 3).clamp(20, 40);
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tree_width), Constraint::Min(1)])
            .split(area)
            .to_vec()
    };
    if cols.len() == 2 {
        let rows = file_tree::build_rows(&files, &app.repo_git_ui.tree_collapsed);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Files")
            .border_style(if app.screen_focus() == 0 {
                Style::default().fg(app.theme.palette.accent)
            } else {
                Style::default().fg(app.theme.palette.border)
            });
        let inner = block.inner(cols[0]);
        frame.render_widget(block, cols[0]);
        let selected = app
            .repo_git_ui
            .tree_selected
            .min(rows.len().saturating_sub(1));
        let lines: Vec<Line<'static>> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| file_tree::render_row(row, &app.theme, index == selected))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        for (index, _) in rows.iter().enumerate() {
            if let Some(y) = inner.y.checked_add(index as u16)
                && y < inner.bottom()
            {
                app.hitmap.register(
                    Rect::new(inner.x, y, inner.width, 1),
                    2,
                    HitAction::RepoGitScreen(RepoGitAction::SelectTreeRow(index)),
                );
            }
        }
        app.repo_git_ui.tree_selected = selected;
    }
    let Some(diff_area) = cols.last().copied() else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Diff")
        .border_style(if app.screen_focus() == 1 {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(diff_area);
    frame.render_widget(block, diff_area);
    if app.repo_git_ui.changes_loading {
        frame.render_widget(
            Paragraph::new("Loading changes…")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            inner,
        );
        return;
    }
    let (lines, _) = diff::render_files(
        &files,
        &app.repo_git_ui.diff_state,
        &app.theme,
        &app.repo_git_ui.highlighter,
        inner.width,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.repo_git_ui.diff_scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_commits(frame: &mut Frame<'_>, area: Rect, app: &mut App, log: &[LogEntry]) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(area.width.min(48)), Constraint::Min(1)])
        .split(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Commits")
        .border_style(if app.screen_focus() == 0 {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(cols[0]);
    frame.render_widget(block, cols[0]);
    let selected = app
        .repo_git_ui
        .commits_selected
        .min(log.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = log
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut style = Style::default().fg(app.theme.palette.fg);
            if index == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(Span::styled(
                format!(
                    "{} {}",
                    &entry.hash[..entry.hash.len().min(7)],
                    entry.subject
                ),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (index, _) in log.iter().enumerate() {
        if let Some(y) = inner.y.checked_add(index as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                HitAction::RepoGitScreen(RepoGitAction::SelectCommit(index)),
            );
        }
    }
    app.repo_git_ui.commits_selected = selected;

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title("Commit diff")
        .border_style(if app.screen_focus() == 1 {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    if let Some(commit) = app.repo_git_ui.commit_detail.clone() {
        let (lines, _) = diff::render_files(
            &commit.files,
            &app.repo_git_ui.commit_diff_state,
            &app.theme,
            &app.repo_git_ui.highlighter,
            detail_inner.width,
        );
        frame.render_widget(
            Paragraph::new(lines).scroll((app.repo_git_ui.commit_diff_scroll as u16, 0)),
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

fn render_branches(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    branches: &[String],
    base_branch: Option<&str>,
) {
    let block = Block::default().borders(Borders::ALL).title("Branches");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let selected = app
        .repo_git_ui
        .branches_selected
        .min(branches.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            let mut style = Style::default().fg(app.theme.palette.fg);
            if index == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let marker = if Some(branch.as_str()) == base_branch {
                " (base)"
            } else {
                ""
            };
            Line::from(Span::styled(format!("{branch}{marker}"), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (index, _) in branches.iter().enumerate() {
        if let Some(y) = inner.y.checked_add(index as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                HitAction::RepoGitScreen(RepoGitAction::SelectBranch(index)),
            );
        }
    }
    app.repo_git_ui.branches_selected = selected;
    if app.repo_git_ui.new_branch_open {
        let width = area.width.min(50);
        let dialog = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + 2,
            width,
            3,
        );
        let dialog_block = Block::default()
            .borders(Borders::ALL)
            .title("New branch name");
        let dialog_inner = dialog_block.inner(dialog);
        frame.render_widget(ratatui::widgets::Clear, dialog);
        frame.render_widget(dialog_block, dialog);
        frame.render_widget(
            Paragraph::new(app.repo_git_ui.new_branch_name.as_str()),
            dialog_inner,
        );
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.repo_git_ui.new_branch_open {
        match key.code {
            KeyCode::Esc => app.repo_git_ui.new_branch_open = false,
            KeyCode::Enter => apply_hit(app, RepoGitAction::NewBranch),
            KeyCode::Backspace => {
                app.repo_git_ui.new_branch_name.pop();
            }
            KeyCode::Char(character) => app.repo_git_ui.new_branch_name.push(character),
            _ => {}
        }
        return true;
    }
    match key.code {
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('[') | KeyCode::Char(']') => {
            let order = [
                RepoGitTab::Commits,
                RepoGitTab::Changes,
                RepoGitTab::Branches,
            ];
            let current = order
                .iter()
                .position(|tab| *tab == app.repo_git_ui.tab)
                .unwrap_or(0);
            let delta = if matches!(key.code, KeyCode::BackTab | KeyCode::Char('[')) {
                -1
            } else {
                1
            };
            let next = (current as i32 + delta).rem_euclid(order.len() as i32) as usize;
            let project = app.repo_git_ui.project.clone();
            open(app, &project, order[next]);
            true
        }
        KeyCode::Char('m') if app.repo_git_ui.tab == RepoGitTab::Changes => {
            apply_hit(app, RepoGitAction::ToggleMode);
            true
        }
        KeyCode::Char('n') if app.repo_git_ui.tab == RepoGitTab::Branches => {
            app.repo_git_ui.new_branch_open = true;
            app.repo_git_ui.new_branch_name.clear();
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.screen_focus() == 1 {
                match app.repo_git_ui.tab {
                    RepoGitTab::Changes => {
                        app.repo_git_ui.diff_scroll = app.repo_git_ui.diff_scroll.saturating_add(1)
                    }
                    RepoGitTab::Commits => {
                        app.repo_git_ui.commit_diff_scroll =
                            app.repo_git_ui.commit_diff_scroll.saturating_add(1)
                    }
                    RepoGitTab::Branches => {}
                }
            } else {
                move_selection(app, 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.screen_focus() == 1 {
                match app.repo_git_ui.tab {
                    RepoGitTab::Changes => {
                        app.repo_git_ui.diff_scroll = app.repo_git_ui.diff_scroll.saturating_sub(1)
                    }
                    RepoGitTab::Commits => {
                        app.repo_git_ui.commit_diff_scroll =
                            app.repo_git_ui.commit_diff_scroll.saturating_sub(1)
                    }
                    RepoGitTab::Branches => {}
                }
            } else {
                move_selection(app, -1);
            }
            true
        }
        KeyCode::Enter if app.repo_git_ui.tab == RepoGitTab::Commits && app.screen_focus() == 0 => {
            apply_hit(
                app,
                RepoGitAction::SelectCommit(app.repo_git_ui.commits_selected),
            );
            true
        }
        KeyCode::Enter if app.repo_git_ui.tab == RepoGitTab::Branches => {
            apply_hit(
                app,
                RepoGitAction::SelectBranch(app.repo_git_ui.branches_selected),
            );
            true
        }
        _ => false,
    }
}

fn move_selection(app: &mut App, delta: i32) {
    match app.repo_git_ui.tab {
        RepoGitTab::Changes => {
            app.repo_git_ui.tree_selected =
                (app.repo_git_ui.tree_selected as i32 + delta).max(0) as usize;
        }
        RepoGitTab::Commits => {
            let next = app.repo_git_ui.commits_selected as i32 + delta;
            app.repo_git_ui.commits_selected = next.max(0) as usize;
            apply_hit(
                app,
                RepoGitAction::SelectCommit(app.repo_git_ui.commits_selected),
            );
        }
        RepoGitTab::Branches => {
            app.repo_git_ui.branches_selected =
                (app.repo_git_ui.branches_selected as i32 + delta).max(0) as usize;
        }
    }
}

pub fn apply_hit(app: &mut App, action: RepoGitAction) {
    match action {
        RepoGitAction::SwitchTab(tab) => {
            let project = app.repo_git_ui.project.clone();
            open(app, &project, tab);
        }
        RepoGitAction::SelectTreeRow(index) => {
            app.repo_git_ui.tree_selected = index;
            let files = app.repo_git_ui.changes_files();
            let rows = file_tree::build_rows(&files, &app.repo_git_ui.tree_collapsed);
            let Some(row) = rows.get(index).cloned() else {
                return;
            };
            use crate::widgets::file_tree::FileTreeRowKind;
            match row.kind {
                FileTreeRowKind::Folder => {
                    if !app.repo_git_ui.tree_collapsed.remove(&row.path) {
                        app.repo_git_ui.tree_collapsed.insert(row.path);
                    }
                }
                FileTreeRowKind::File(_) => {
                    if let Some(key) = &row.file_key {
                        app.repo_git_ui.diff_state.toggle_file(key);
                    }
                }
            }
        }
        RepoGitAction::SelectCommit(index) => {
            app.repo_git_ui.commits_selected = index;
            let Some(RepoResponse::Present(present)) = &app.repo_git_ui.repo else {
                return;
            };
            let Some(entry) = present.log.get(index) else {
                return;
            };
            app.pending.push(PendingAction::LoadRepoGitCommitDiff {
                project: app.repo_git_ui.project.clone(),
                sha: entry.hash.clone(),
            });
        }
        RepoGitAction::SelectBranch(index) => {
            app.repo_git_ui.branches_selected = index;
        }
        RepoGitAction::ToggleMode => {
            app.repo_git_ui.diff_state.mode = app.repo_git_ui.diff_state.mode.toggled();
        }
        RepoGitAction::ToggleWrap => {
            app.repo_git_ui.diff_state.wrap = !app.repo_git_ui.diff_state.wrap;
        }
        RepoGitAction::NewBranch => {
            app.repo_git_ui.new_branch_open = false;
            let name = app.repo_git_ui.new_branch_name.trim().to_owned();
            if name.is_empty() {
                return;
            }
            let from = app.repo_git_ui.branches_selected_name().filter(|_| true);
            app.pending.push(PendingAction::RepoGitBranch {
                project: app.repo_git_ui.project.clone(),
                name,
                from,
            });
        }
    }
}

impl RepoGitUi {
    fn changes_files(&self) -> Vec<coducktor_contract::ChangedFile> {
        // Populated separately from `repo.changes` — see `PendingAction::LoadRepoGit`'s
        // handler, which stores the structured `/repo/changes` payload's files here via
        // `repo_changes`. Kept as a method (not a field lookup) so the render/apply-hit call
        // sites above stay agnostic of where the payload lives.
        self.repo_changes_files.clone()
    }

    fn branches_selected_name(&self) -> Option<String> {
        let RepoResponse::Present(present) = self.repo.as_ref()? else {
            return None;
        };
        present.branches.get(self.branches_selected).cloned()
    }
}
