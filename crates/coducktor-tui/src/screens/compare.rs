//! The compare-variants screen — side-by-side (or stacked, when narrow) columns
//! per variant: progress excerpt, diff stat, and a `Pick` action. The selected column's full
//! structured diff loads lazily through the same `Engine::run_changes` route the task-git
//! Changes tab uses (compare's own contract, `GroupVariant`, only carries `diffStat` as raw
//! `git diff --stat` text — see `runs.rs`'s doc comment on that field — so the per-file patch
//! for the full diff uses the run's own changes operation.

use std::collections::HashMap;

use coducktor_contract::{ChangesPayload, GroupResponse, GroupVariant};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, Route};
use crate::diff::{self, DiffViewState, Highlighter};
use crate::input::hitmap::{CompareAction, HitAction};

pub struct CompareUi {
    pub project: String,
    pub group_id: String,
    pub group: Option<GroupResponse>,
    pub selected: usize,
    pub variant_diffs: HashMap<String, ChangesPayload>,
    pub diff_state: DiffViewState,
    pub diff_scroll: usize,
    pub highlighter: Highlighter,
}

impl Default for CompareUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            group_id: String::new(),
            group: None,
            selected: 0,
            variant_diffs: HashMap::new(),
            diff_state: DiffViewState::default(),
            diff_scroll: 0,
            highlighter: Highlighter::new(),
        }
    }
}

pub fn open(app: &mut App, project: &str, group_id: &str) {
    if app.compare_ui.project != project || app.compare_ui.group_id != group_id {
        app.compare_ui = CompareUi {
            project: project.to_owned(),
            group_id: group_id.to_owned(),
            ..CompareUi::default()
        };
    }
    app.navigate_route(Route::Compare {
        project: project.to_owned(),
        group_id: group_id.to_owned(),
    });
    app.pending.push(PendingAction::LoadCompare {
        project: project.to_owned(),
        group_id: group_id.to_owned(),
    });
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(group) = app.compare_ui.group.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    if group.runs.is_empty() {
        frame.render_widget(
            Paragraph::new("No variants in this group.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    }

    // Stacked when narrow, side-by-side otherwise.
    let column_width = 34u16;
    let side_by_side = area.width >= column_width * group.runs.len() as u16;
    let column_count = group.runs.len();
    let constraints: Vec<Constraint> = if side_by_side {
        (0..column_count)
            .map(|_| Constraint::Ratio(1, column_count as u32))
            .collect()
    } else {
        vec![Constraint::Length(6); column_count]
    };
    let direction = if side_by_side {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };

    if side_by_side {
        let columns = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(area);
        for (index, variant) in group.runs.iter().enumerate() {
            render_column(
                frame,
                columns[index],
                app,
                variant,
                index == app.compare_ui.selected,
            );
        }
    } else {
        let rows = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(Rect::new(
                area.x,
                area.y,
                area.width,
                area.height.min(6 * column_count as u16),
            ))
            .to_vec();
        for (index, variant) in group.runs.iter().enumerate() {
            if let Some(row) = rows.get(index) {
                render_column(frame, *row, app, variant, index == app.compare_ui.selected);
            }
        }
    }

    // Render the selected variant's full diff below the columns, once, given the terminal's
    // limited width budget.
    let diff_area = Rect::new(
        area.x,
        area.y.saturating_add(
            if side_by_side {
                column_count as u16 * 2
            } else {
                6 * column_count as u16
            }
            .min(area.height),
        ),
        area.width,
        area.height.saturating_sub(if side_by_side {
            8
        } else {
            6 * column_count as u16
        }),
    );
    if diff_area.height > 0
        && let Some(variant) = group.runs.get(app.compare_ui.selected)
    {
        render_selected_diff(frame, diff_area, app, variant);
    }
}

fn status_label(status: coducktor_contract::RunStatus) -> &'static str {
    use coducktor_contract::RunStatus::*;
    match status {
        Queued => "queued",
        Running => "running",
        Idle => "idle",
        Waiting => "waiting",
        Review => "review",
        Done => "done",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

fn render_column(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    variant: &GroupVariant,
    selected: bool,
) {
    let title = format!(
        "Variant {} — {}",
        variant.variant,
        status_label(variant.status)
    );
    let mut style = Style::default().fg(app.theme.palette.fg);
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if selected {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![
        Line::from(Span::styled(variant.title.clone(), style)),
        Line::from(Span::styled(
            format!("{} tokens", variant.tokens_used as i64),
            Style::default().fg(app.theme.palette.soft_fg),
        )),
    ];
    for stat_line in variant
        .diff_stat
        .lines()
        .take((inner.height as usize).saturating_sub(3))
    {
        lines.push(Line::from(Span::styled(
            stat_line.to_owned(),
            Style::default().fg(app.theme.palette.soft_fg),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    app.hitmap.register(
        area,
        2,
        HitAction::CompareScreen(CompareAction::Pick(pick_index_of(app, variant))),
    );
}

fn pick_index_of(app: &App, variant: &GroupVariant) -> usize {
    app.compare_ui
        .group
        .as_ref()
        .and_then(|group| group.runs.iter().position(|run| run.id == variant.id))
        .unwrap_or(0)
}

fn render_selected_diff(frame: &mut Frame<'_>, area: Rect, app: &mut App, variant: &GroupVariant) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Full diff — variant {}", variant.variant));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(changes) = app.compare_ui.variant_diffs.get(&variant.id).cloned() else {
        frame.render_widget(
            Paragraph::new("Loading diff…").style(Style::default().fg(app.theme.palette.soft_fg)),
            inner,
        );
        return;
    };
    let (lines, _) = diff::render_files(
        &changes.files,
        &app.compare_ui.diff_state,
        &app.theme,
        &app.compare_ui.highlighter,
        inner.width,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.compare_ui.diff_scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(group) = app.compare_ui.group.clone() else {
        return false;
    };
    match key.code {
        KeyCode::Right => {
            if !group.runs.is_empty() {
                app.compare_ui.selected = (app.compare_ui.selected + 1) % group.runs.len();
                load_selected_diff(app);
            }
            true
        }
        KeyCode::Left => {
            if !group.runs.is_empty() {
                app.compare_ui.selected =
                    (app.compare_ui.selected + group.runs.len() - 1) % group.runs.len();
                load_selected_diff(app);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.compare_ui.diff_scroll = app.compare_ui.diff_scroll.saturating_add(1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.compare_ui.diff_scroll = app.compare_ui.diff_scroll.saturating_sub(1);
            true
        }
        KeyCode::Enter => {
            apply_hit(app, CompareAction::Pick(app.compare_ui.selected));
            true
        }
        _ => false,
    }
}

fn load_selected_diff(app: &mut App) {
    let Some(group) = app.compare_ui.group.clone() else {
        return;
    };
    let Some(variant) = group.runs.get(app.compare_ui.selected) else {
        return;
    };
    if app.compare_ui.variant_diffs.contains_key(&variant.id) {
        return;
    }
    app.pending.push(PendingAction::LoadCompareVariantDiff {
        project: app.compare_ui.project.clone(),
        group_id: app.compare_ui.group_id.clone(),
        run_id: variant.id.clone(),
    });
}

pub(crate) fn jump_selection(app: &mut App, end: bool) {
    let last = app
        .compare_ui
        .group
        .as_ref()
        .map(|group| group.runs.len().saturating_sub(1))
        .unwrap_or(0);
    app.compare_ui.selected = if end { last } else { 0 };
    load_selected_diff(app);
}

pub fn apply_hit(app: &mut App, action: CompareAction) {
    match action {
        CompareAction::Pick(index) => {
            let Some(group) = app.compare_ui.group.clone() else {
                return;
            };
            let Some(variant) = group.runs.get(index) else {
                return;
            };
            app.pending.push(PendingAction::PickVariant {
                project: app.compare_ui.project.clone(),
                group_id: app.compare_ui.group_id.clone(),
                run_id: variant.id.clone(),
            });
        }
    }
}
