//! Tasks overview screen — `screens/tasks.rs`.
//!
//! Behavioural spec: `packages/web/src/routes/tasks-overview.tsx` plus the pure
//! helpers ported into `runs_util`. Layout: title row (Active/Archived segments,
//! count-gated actions, search), the run table, and compare-variant strips below.

use coducktor_contract::{ApiRun, ProcessUsage, RunStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, ConfirmRequest, PendingAction, RowMenu, RowMenuItem};
use crate::input::hitmap::HitAction;
use crate::screens::runs_util::{
    Attention, TaskView, UsageKind, attention, clock_time, compare_groups, filter_runs,
    finished_run_count, format_cost, format_diff, format_tokens, is_read_done_item, is_unread,
    queue_positions, run_title, short_age, sort_runs, task_reference, unread_done_count,
    usage_cells, workflow_label,
};
use crate::theme::Theme;
use crate::widgets::table::{ColumnId, SortState, Table, TableCell, TableRow};

/// The column set for the per-project table (§8.1).
pub const COLUMNS: [ColumnId; 11] = [
    ColumnId::Status,
    ColumnId::Task,
    ColumnId::Workflow,
    ColumnId::Branch,
    ColumnId::Diff,
    ColumnId::Reference,
    ColumnId::Tokens,
    ColumnId::Cost,
    ColumnId::Cpu,
    ColumnId::Memory,
    ColumnId::Started,
];

/// Screen-local state: search, table widget state, and the sort picker.
#[derive(Debug, Clone)]
pub struct TasksUi {
    pub query: String,
    pub table: Table,
    pub sort_picker: bool,
}

impl Default for TasksUi {
    fn default() -> Self {
        let mut table = Table::with_columns(COLUMNS.to_vec());
        // The web's default column set folds only Branch (§8.1, `lib/task-columns.ts`).
        table.folded.insert(ColumnId::Branch);
        Self {
            query: String::new(),
            table,
            sort_picker: false,
        }
    }
}

/// Build the ordered, filtered rows the table should render.
pub fn build_rows(
    runs: &[ApiRun],
    view: TaskView,
    query: &str,
    sort: Option<SortState>,
    now: i64,
    theme: &Theme,
    live_usage: &std::collections::BTreeMap<String, ProcessUsage>,
) -> Vec<TableRow> {
    let filtered = filter_runs(runs, query);
    let mut order = sort_runs(runs, view);
    if let Some(sort) = sort {
        order.sort_by(|&a, &b| {
            let ordering = compare_cells(&runs[a], &runs[b], sort.column);
            if sort.descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }
    order.retain(|index| filtered.iter().any(|run| std::ptr::eq(*run, &runs[*index])));
    let positions = queue_positions(runs);
    order
        .into_iter()
        .map(|index| {
            let run = &runs[index];
            let cells = run_cells(
                run,
                positions.get(&run.record.id).copied(),
                now,
                theme,
                live_usage.get(&run.record.id),
            );
            TableRow {
                key: run.record.id.clone(),
                cells,
            }
        })
        .collect()
}

/// The order one cell column imposes — used by header sort.
fn compare_cells(a: &ApiRun, b: &ApiRun, column: ColumnId) -> std::cmp::Ordering {
    let a = &a.record;
    let b = &b.record;
    match column {
        ColumnId::Status => attention_status_order(a.status).cmp(&attention_status_order(b.status)),
        ColumnId::Started => a.started_at.cmp(&b.started_at),
        ColumnId::Tokens => a.tokens_used.total_cmp(&b.tokens_used),
        ColumnId::Cost => a
            .cost_usd
            .unwrap_or_default()
            .total_cmp(&b.cost_usd.unwrap_or_default()),
        ColumnId::Workflow => a.workflow.cmp(&b.workflow),
        ColumnId::Branch => a.branch.cmp(&b.branch),
        _ => a.created_at.cmp(&b.created_at),
    }
}

fn attention_status_order(status: RunStatus) -> u8 {
    match status {
        RunStatus::Waiting => 0,
        RunStatus::Review => 1,
        RunStatus::Running => 2,
        RunStatus::Queued => 3,
        RunStatus::Done => 4,
        RunStatus::Failed => 5,
        RunStatus::Cancelled => 6,
    }
}

/// One row's cells, in `COLUMNS` order.
fn run_cells(
    run: &ApiRun,
    queue_position: Option<usize>,
    now: i64,
    theme: &Theme,
    live: Option<&ProcessUsage>,
) -> Vec<TableCell> {
    let record = &run.record;
    let att = attention(run);
    let status_text = status_text(run, queue_position, att);
    let unread = is_unread(run);
    let read_done = is_read_done_item(run);
    let title = run_title(run);
    let title_style = if unread {
        Style::default()
            .fg(theme.palette.fg)
            .add_modifier(Modifier::BOLD)
    } else if read_done {
        Style::default().fg(theme.palette.soft_fg)
    } else {
        Style::default().fg(theme.palette.fg)
    };
    let reference = task_reference(run);
    let (cpu, mem) = usage_cells(run, live);

    let mut cells = Vec::with_capacity(COLUMNS.len());
    cells.push(TableCell::new(status_text, att.tone.style(theme)));
    cells.push(TableCell::new(
        if unread {
            format!("● {title}")
        } else {
            title
        },
        title_style,
    ));
    cells.push(TableCell::new(
        workflow_label(run),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        record.branch.clone().unwrap_or_default(),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        format_diff(record.diff_stat.as_ref()),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        reference
            .as_ref()
            .map(|reference| format!("{} #{}", reference.kind, reference.number))
            .unwrap_or_default(),
        Style::default().fg(theme.palette.review),
    ));
    cells.push(TableCell::new(
        format_tokens(record.input_tokens, record.output_tokens),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        format_cost(record.cost_usd),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        if queue_position.is_some() {
            String::new()
        } else {
            cpu.text.clone()
        },
        if queue_position.is_some() {
            Style::default().fg(theme.palette.soft_fg)
        } else if cpu.kind == UsageKind::Live {
            Style::default().fg(theme.palette.fg)
        } else {
            Style::default().fg(theme.palette.soft_fg)
        },
    ));
    cells.push(TableCell::new(
        mem.text.clone(),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells.push(TableCell::new(
        short_age(
            record.started_at.as_deref().unwrap_or(&record.created_at),
            now,
        ),
        Style::default().fg(theme.palette.soft_fg),
    ));
    cells
}

fn status_text(run: &ApiRun, queue_position: Option<usize>, att: Attention) -> String {
    if let Some(position) = queue_position {
        return format!("queued #{position}");
    }
    if att.label == "scheduled"
        && let Some(resume) = run.record.auto_resume_at.as_deref().and_then(clock_time)
    {
        return format!("sched {resume}");
    }
    att.label.to_owned()
}

/// Render the whole screen: title row, table, compare strips.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let theme = app.theme;
    let view = app.task_view();
    let now = app.now_epoch;
    let project = app.current_project().to_owned();
    let compare_height = compare_groups(&app.tasks, view).len() as u16;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(compare_height.min(3)),
        ])
        .split(area);
    render_title_row(frame, layout[0], app, view);

    let scroll_y = app.tasks_ui.table.scroll_y;
    let selected = app.tasks_ui.table.selected;
    app.tasks_ui.table.hover = hover_row(app.hover, layout[1], scroll_y);
    app.tasks_ui.table.rows = build_rows(
        &app.tasks,
        view,
        &app.tasks_ui.query,
        app.tasks_ui.table.sort,
        now,
        &theme,
        &app.live_usage,
    );
    app.tasks_ui.table.select(selected);
    app.tasks_ui.table.render(
        frame,
        layout[1],
        &mut app.hitmap,
        &format!("TASKS — {project}"),
        if app.tasks_ui.query.trim().is_empty() {
            "No tasks yet. Describe a task to get started."
        } else {
            "No tasks match your search."
        },
    );
    render_compare_strips(frame, layout[2], app, view);
}

fn render_title_row(frame: &mut Frame<'_>, area: Rect, app: &mut App, view: TaskView) {
    let theme = app.theme;
    let runs = &app.tasks;
    let active = runs.iter().filter(|run| !run.record.archived).count();
    let archived = runs.iter().filter(|run| run.record.archived).count();
    let finished = finished_run_count(runs);
    let unread = unread_done_count(runs);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(span(
        " Tasks ",
        view_style(theme, view == TaskView::Active, active > 0),
    ));
    spans.push(span(
        &format!("Archived {archived}"),
        view_style(theme, view == TaskView::Archived, false),
    ));
    spans.push(Span::raw("  "));
    if view == TaskView::Active {
        if unread > 0 {
            spans.push(span("Mark all read", action_style(theme, unread > 0)));
        }
        if finished > 0 {
            spans.push(span("Archive finished", action_style(theme, finished > 0)));
        }
        spans.push(Span::raw("  "));
    }
    spans.push(span(
        &format!("/ {}", app.tasks_ui.query),
        Style::default().fg(theme.palette.soft_fg),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(spans.clone())).style(Style::default().bg(theme.palette.surface)),
        area,
    );
    let hitmap = &mut app.hitmap;
    // Active / Archived segments.
    hitmap.register(
        Rect::new(area.x, area.y, 9, area.height),
        3,
        HitAction::ActiveTasks,
    );
    hitmap.register(
        Rect::new(area.x + 9, area.y, 14, area.height),
        3,
        HitAction::ArchivedTasks,
    );
    let mut cursor = area.x + 9 + 14 + 2;
    if view == TaskView::Active && unread > 0 {
        hitmap.register(
            Rect::new(cursor, area.y, 13, area.height),
            3,
            HitAction::MarkAllRead,
        );
        cursor += 13 + 2;
    }
    if view == TaskView::Active && finished > 0 {
        hitmap.register(
            Rect::new(cursor, area.y, 16, area.height),
            3,
            HitAction::ArchiveFinished,
        );
    }
}

fn render_compare_strips(frame: &mut Frame<'_>, area: Rect, app: &mut App, view: TaskView) {
    if area.height == 0 {
        return;
    }
    let groups = compare_groups(&app.tasks, view);
    if groups.is_empty() {
        return;
    }
    let lines: Vec<Line<'static>> = groups
        .iter()
        .map(|group| {
            Line::from(vec![
                Span::styled(
                    format!("  {} — {} variants finished", group.title, group.count),
                    Style::default().fg(app.theme.palette.soft_fg),
                ),
                Span::styled("  Compare", Style::default().fg(app.theme.palette.accent)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().bg(app.theme.palette.surface)),
        area,
    );
}

/// Route mouse/keyboard interactions from the table back into the app.
pub fn handle_table_hit(app: &mut App, action: HitAction) {
    match action {
        HitAction::TableRow(index) => {
            let Some(row) = app.tasks_ui.table.rows.get(index) else {
                return;
            };
            let id = row.key.clone();
            open_thread(app, &id);
        }
        HitAction::TableHeader(column) => toggle_sort(app, column),
        _ => {}
    }
}

/// Fold/unfold the header column under the pointer (`x`). Falls back to the
/// first foldable column when nothing is hovered.
fn toggle_hovered_fold(app: &mut App) {
    let hovered = app.hover.and_then(|(column, row)| {
        app.tasks_ui
            .table
            .header_hits
            .iter()
            .find(|(_, rect)| {
                column >= rect.x && column < rect.x.saturating_add(rect.width) && row == rect.y
            })
            .map(|(column, _)| *column)
    });
    let column = hovered.or_else(|| {
        app.tasks_ui
            .table
            .columns
            .iter()
            .copied()
            .find(|column| column.foldable())
    });
    if let Some(column) = column {
        app.tasks_ui.table.toggle_fold(column);
    }
}

fn toggle_sort(app: &mut App, column: ColumnId) {
    let current = app.tasks_ui.table.sort;
    let sort = match current {
        Some(SortState {
            column: existing,
            descending,
        }) if existing == column => SortState {
            column,
            descending: !descending,
        },
        _ => SortState {
            column,
            descending: false,
        },
    };
    app.tasks_ui.table.sort = Some(sort);
}

/// The keyboard contract for the Tasks screen (§8.1). Returns true when consumed.
pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;
    if app.tasks_ui.sort_picker {
        return handle_sort_picker_key(app, key);
    }
    if let Some(menu) = app.row_menu.clone() {
        return crate::app::handle_row_menu_key(app, &menu, key);
    }
    match key.code {
        KeyCode::Char('j') => {
            app.tasks_ui.table.move_selection(1);
            true
        }
        KeyCode::Char('k') => {
            app.tasks_ui.table.move_selection(-1);
            true
        }
        KeyCode::Char('t') => {
            app.toggle_view();
            true
        }
        KeyCode::Char('s') => {
            app.tasks_ui.sort_picker = true;
            true
        }
        KeyCode::Char('x') => {
            toggle_hovered_fold(app);
            true
        }
        KeyCode::Char('[') => {
            app.tasks_ui.table.scroll_columns(-1);
            true
        }
        KeyCode::Char(']') => {
            app.tasks_ui.table.scroll_columns(1);
            true
        }
        KeyCode::Char('/') => {
            app.filter_mode = true;
            true
        }
        KeyCode::Char('a') => {
            archive_selected(app, true);
            true
        }
        KeyCode::Char('r') => {
            toggle_read(app);
            true
        }
        KeyCode::Char('d') => {
            request_delete(app);
            true
        }
        KeyCode::Char('p') => {
            open_selected_pr(app);
            true
        }
        KeyCode::Char('o') => {
            app.notice = Some("diff view lands in A9".to_owned());
            true
        }
        KeyCode::Enter => {
            if let Some((_, row)) = app.tasks_ui.table.selected_row() {
                let id = row.key.clone();
                open_thread(app, &id);
            }
            true
        }
        KeyCode::Esc => true,
        _ => false,
    }
}

fn handle_sort_picker_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.sort_picker_index = (app.sort_picker_index + 1).min(SORT_PICKER.len() - 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.sort_picker_index = app.sort_picker_index.saturating_sub(1);
        }
        KeyCode::Enter => {
            let column = SORT_PICKER[app.sort_picker_index];
            let current = app.tasks_ui.table.sort;
            let sort = match current {
                Some(SortState {
                    column: existing,
                    descending,
                }) if existing == column => SortState {
                    column,
                    descending: !descending,
                },
                _ => SortState {
                    column,
                    descending: false,
                },
            };
            app.tasks_ui.table.sort = Some(sort);
            app.tasks_ui.sort_picker = false;
            app.sort_picker_index = 0;
        }
        KeyCode::Esc => {
            app.tasks_ui.sort_picker = false;
            app.sort_picker_index = 0;
        }
        _ => {}
    }
    true
}

/// The columns `s` offers, in picker order.
const SORT_PICKER: [ColumnId; 5] = [
    ColumnId::Status,
    ColumnId::Started,
    ColumnId::Tokens,
    ColumnId::Cost,
    ColumnId::Workflow,
];

fn archive_selected(app: &mut App, archived: bool) {
    let Some((_, row)) = app.tasks_ui.table.selected_row() else {
        return;
    };
    let project = app.current_project().to_owned();
    app.pending.push(PendingAction::Archive {
        project,
        id: row.key.clone(),
        archived,
    });
}

fn toggle_read(app: &mut App) {
    let Some((_, row)) = app.tasks_ui.table.selected_row() else {
        return;
    };
    let run = app
        .tasks
        .iter()
        .find(|run| run.record.id == row.key)
        .cloned();
    let Some(run) = run else {
        return;
    };
    let project = app.current_project().to_owned();
    app.pending.push(if is_unread(&run) {
        PendingAction::Read {
            project,
            id: row.key.clone(),
        }
    } else {
        PendingAction::Unread {
            project,
            id: row.key.clone(),
        }
    });
}

fn request_delete(app: &mut App) {
    let Some((_, row)) = app.tasks_ui.table.selected_row() else {
        return;
    };
    let run = app.tasks.iter().find(|run| run.record.id == row.key);
    let Some(run) = run else {
        return;
    };
    if matches!(
        run.record.status,
        RunStatus::Queued | RunStatus::Running | RunStatus::Waiting
    ) {
        app.notice = Some("run is active — cancel it first".to_owned());
        return;
    }
    app.confirm = Some(ConfirmRequest {
        text: format!("Delete \"{}\" and its branch?", run_title(run)),
        action: PendingAction::Delete {
            project: app.current_project().to_owned(),
            id: row.key.clone(),
        },
    });
}

fn open_selected_pr(app: &mut App) {
    let Some((_, row)) = app.tasks_ui.table.selected_row() else {
        return;
    };
    let Some(run) = app.tasks.iter().find(|run| run.record.id == row.key) else {
        return;
    };
    let Some(reference) = task_reference(run) else {
        app.notice = Some("no PR or issue reference on this task".to_owned());
        return;
    };
    let Some(url) = reference.url else {
        app.notice = Some(format!(
            "no URL known for {} #{}",
            reference.kind, reference.number
        ));
        return;
    };
    app.open_url(&url);
}

pub fn open_thread(app: &mut App, id: &str) {
    let project = app.current_project().to_owned();
    app.history.navigate(crate::app::Route::Thread {
        project,
        id: id.to_owned(),
    });
}

/// Set the row menu for the selected row.
pub fn open_row_menu(app: &mut App) {
    let Some((_, row)) = app.tasks_ui.table.selected_row() else {
        return;
    };
    let Some(run) = app
        .tasks
        .iter()
        .find(|run| run.record.id == row.key)
        .cloned()
    else {
        return;
    };
    let archived = run.record.archived;
    let mut items = vec![
        RowMenuItem {
            label: "Open thread".to_owned(),
            action: crate::app::MenuAction::Open,
        },
        RowMenuItem {
            label: if archived {
                "Restore to active".to_owned()
            } else {
                "Archive".to_owned()
            },
            action: if archived {
                crate::app::MenuAction::Restore
            } else {
                crate::app::MenuAction::Archive
            },
        },
    ];
    if can_be_unread_or_read(&run) {
        items.push(RowMenuItem {
            label: if is_unread(&run) {
                "Mark read".to_owned()
            } else {
                "Mark unread".to_owned()
            },
            action: if is_unread(&run) {
                crate::app::MenuAction::MarkRead
            } else {
                crate::app::MenuAction::MarkUnread
            },
        });
    }
    if task_reference(&run).is_some() {
        items.push(RowMenuItem {
            label: "Open PR/issue".to_owned(),
            action: crate::app::MenuAction::OpenPr,
        });
    }
    if !run.record.branch.as_deref().is_none_or(str::is_empty) {
        items.push(RowMenuItem {
            label: "Copy branch".to_owned(),
            action: crate::app::MenuAction::CopyBranch,
        });
    }
    items.push(RowMenuItem {
        label: "Delete".to_owned(),
        action: crate::app::MenuAction::Delete,
    });
    let project = app.current_project().to_owned();
    app.row_menu = Some(RowMenu {
        project,
        run_id: row.key.clone(),
        title: run_title(&run),
        items,
        selected: 0,
    });
}

fn can_be_unread_or_read(run: &ApiRun) -> bool {
    !run.record.archived
        && matches!(
            run.record.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        )
}

fn view_style(theme: Theme, active: bool, _attention: bool) -> Style {
    if active {
        Style::default()
            .fg(theme.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.palette.soft_fg)
    }
}

fn action_style(theme: Theme, _enabled: bool) -> Style {
    Style::default().fg(theme.palette.soft_fg)
}

fn span(text: &str, style: Style) -> Span<'static> {
    Span::styled(text.to_owned(), style)
}

/// Map a hover position to the row under the pointer, or `None`.
fn hover_row(hover: Option<(u16, u16)>, area: Rect, scroll_y: usize) -> Option<usize> {
    let (column, row) = hover?;
    if column < area.x || column >= area.x.saturating_add(area.width) {
        return None;
    }
    if row < area.y.saturating_add(2) {
        return None;
    }
    Some(scroll_y + usize::from(row - (area.y + 2)))
}

#[cfg(test)]
mod tests {
    use coducktor_contract::{RunRecord, RunStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::App;
    use crate::input::keymap::Keymap;

    fn api_run(index: u8, status: RunStatus, branch: Option<&str>) -> ApiRun {
        ApiRun {
            record: RunRecord {
                id: format!("run-{index}"),
                title: format!("Task {index} — a reasonably long title to truncate"),
                workflow: "quick-task".to_owned(),
                task: String::new(),
                status,
                created_at: "2026-08-15T00:00:00Z".to_owned(),
                started_at: Some("2026-08-15T00:00:00Z".to_owned()),
                tokens_used: 12_345.0,
                cost_usd: Some(0.42),
                archived: false,
                branch: branch.map(ToOwned::to_owned),
                diff_stat: Some(coducktor_contract::DiffStat {
                    adds: 12.0,
                    dels: 3.0,
                    files: 2.0,
                    repointed: None,
                }),
                steps: Vec::new(),
                ..RunRecord::default()
            },
            usage: None,
        }
    }

    fn app_with_tasks(tasks: Vec<ApiRun>) -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.now_epoch = 1_800_000_000;
        app.set_tasks(tasks);
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn tasks_table_renders_statuses_and_usage() {
        let mut app = app_with_tasks(vec![
            api_run(1, RunStatus::Running, Some("feat/shell")),
            api_run(2, RunStatus::Waiting, None),
            api_run(3, RunStatus::Done, None),
        ]);
        app.tasks_ui.table.folded.remove(&ColumnId::Branch);
        let content = render(&mut app, 160, 30);
        assert!(content.contains("running"));
        assert!(content.contains("needs you"));
        assert!(content.contains("done"));
        assert!(content.contains("feat/shell"));
        assert!(content.contains("$0.42"));
        assert!(content.contains("+12 −3"));
    }

    #[test]
    fn queued_run_shows_its_queue_position() {
        let mut app = app_with_tasks(vec![api_run(1, RunStatus::Queued, None)]);
        let content = render(&mut app, 160, 30);
        assert!(content.contains("queued #1"), "got: {content}");
    }

    #[test]
    fn search_filters_the_rows() {
        let mut app = app_with_tasks(vec![
            api_run(1, RunStatus::Running, Some("feat/shell")),
            api_run(2, RunStatus::Running, None),
        ]);
        app.tasks_ui.query = "feat/shell".to_owned();
        let content = render(&mut app, 160, 30);
        assert!(content.contains("Task 1"));
        assert!(!content.contains("Task 2"));
    }

    #[test]
    fn snapshot_tasks_table_at_three_sizes() {
        let mut app = app_with_tasks(vec![
            api_run(1, RunStatus::Running, Some("feat/shell")),
            api_run(2, RunStatus::Waiting, None),
            api_run(3, RunStatus::Done, None),
        ]);
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("tasks_table_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
