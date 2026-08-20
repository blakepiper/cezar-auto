//! The GitHub screen: Issues and Pull Requests, item details, and the hand-to-agent action.
//! Three panes: the Issues/Pull-requests tab strip, the item list,
//! and the detail (title, labels, markdown body, comment/timeline, check rollup, the
//! hand-to-agent card). PR detail adds a Changes tab and a Merge action with its confirm dialog.
//!
//! Every surface degrades per the `{available, reason}` contract — the aggregate read, the
//! comments read, the merge gate and the changes read each render their reason verbatim
//! when `gh` is absent or the forge is unreachable. Never an error screen.

use coducktor_contract::{
    ChangedFile, ChangedFileStatus, ChecksGlyph, CreateRunInput, GithubCommentsData, GithubData,
    GithubItem, GithubMergeMethod, GithubPrChangesData, GithubPrMergeStateResponse, Skill,
    WorkflowDef, WorkflowStepDef,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, Route};
use crate::diff::{DiffViewState, Highlighter, render_files};
use crate::input::hitmap::{GithubAction, HitAction};
use crate::markdown::RenderCache;
use crate::theme::Theme;

/// A skills-as-chain hand-off carries at most 8 steps.
const MAX_CHAIN_STEPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubTab {
    Issues,
    Prs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubDetailTab {
    Thread,
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubFocus {
    Tab,
    List,
    Detail,
    SkillPicker,
}

/// Engine-fetched state for the open GitHub screen.
pub struct GithubUi {
    pub project: String,
    pub data: Option<GithubData>,
    pub tab: GithubTab,
    pub list_selected: usize,
    pub focus: GithubFocus,

    pub detail_item: Option<GithubItem>,
    pub detail_tab: GithubDetailTab,
    pub comments: Option<GithubCommentsData>,
    pub merge_state: Option<GithubPrMergeStateResponse>,
    pub pr_changes: Option<GithubPrChangesData>,
    pub changes_diff: DiffViewState,
    pub changes_scroll: usize,
    pub comments_scroll: usize,
    pub merge_method: GithubMergeMethod,
    pub queued: Option<String>,

    // Hand-to-agent pickers.
    pub workflows: Vec<WorkflowDef>,
    pub skills: Vec<Skill>,
    pub picked_workflow: Option<usize>,
    pub picked_skills: Vec<usize>,
    pub skill_query: String,

    pub markdown: RenderCache,
    pub highlighter: Highlighter,
}

impl Default for GithubUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            data: None,
            tab: GithubTab::Issues,
            list_selected: 0,
            focus: GithubFocus::Tab,
            detail_item: None,
            detail_tab: GithubDetailTab::Thread,
            comments: None,
            merge_state: None,
            pr_changes: None,
            changes_diff: DiffViewState::default(),
            changes_scroll: 0,
            comments_scroll: 0,
            merge_method: GithubMergeMethod::Squash,
            queued: None,
            workflows: Vec::new(),
            skills: Vec::new(),
            picked_workflow: None,
            picked_skills: Vec::new(),
            skill_query: String::new(),
            markdown: RenderCache::new(),
            highlighter: Highlighter::new(),
        }
    }
}

/// Navigate to the GitHub screen and queue its aggregate read + picker data.
pub fn open(app: &mut App, project: &str) {
    if app.github_ui.project != project {
        app.github_ui = GithubUi {
            project: project.to_owned(),
            ..GithubUi::default()
        };
    }
    app.request_navigate(Route::Github {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadGithub {
        project: project.to_owned(),
    });
    app.pending.push(PendingAction::LoadGithubPickers {
        project: project.to_owned(),
    });
}

fn items(ui: &GithubUi) -> &[GithubItem] {
    let Some(data) = &ui.data else {
        return &[];
    };
    match ui.tab {
        GithubTab::Issues => &data.issues,
        GithubTab::Prs => &data.prs,
    }
}

fn checks_glyph(item: &GithubItem) -> Option<&str> {
    item.checks.flatten().map(|glyph| match glyph {
        ChecksGlyph::Passing => "✓",
        ChecksGlyph::Failing => "✗",
        ChecksGlyph::Pending => "…",
    })
}

/// Build the item's task reference — verb, `#N`, title, and URL — with no body quoted. The
/// wording is load-bearing because task-marker parsing recovers PR/issue attribution from it.
pub fn github_task_ref(item: &GithubItem) -> String {
    let verb = match item.kind {
        coducktor_contract::GithubItemKind::Pr => "Address GitHub pull request",
        coducktor_contract::GithubItemKind::Issue => "Fix GitHub issue",
    };
    format!("{verb} #{}: {}\n\n{}", item.number, item.title, item.url)
}

/// Build one `{{task}}` step per skill, deduplicating ids and capping at `MAX_CHAIN_STEPS`.
pub fn skill_chain_steps(names: &[String]) -> Vec<WorkflowStepDef> {
    let mut steps: Vec<WorkflowStepDef> = Vec::new();
    for name in names.iter().take(MAX_CHAIN_STEPS) {
        let used: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        let mut id = name.clone();
        let mut n = 2;
        while used.contains(&id.as_str()) {
            id = format!("{name}-{n}");
            n += 1;
        }
        steps.push(WorkflowStepDef {
            id,
            name: Some(name.clone()),
            prompt: Some("{{task}}".to_owned()),
            skill: Some(name.clone()),
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        });
    }
    steps
}

/// The run request for one hand-off, including the selected GitHub task context.
fn handoff_run_body(ui: &GithubUi, item: &GithubItem) -> CreateRunInput {
    let task = github_task_ref(item);
    let skill_names: Vec<String> = ui
        .picked_skills
        .iter()
        .filter_map(|index| ui.skills.get(*index))
        .map(|skill| skill.name.clone())
        .collect();
    if let Some(index) = ui.picked_workflow
        && let Some(workflow) = ui.workflows.get(index)
    {
        return CreateRunInput {
            workflow: Some(workflow.name.clone()),
            task,
            ..CreateRunInput::default()
        };
    }
    if !skill_names.is_empty() {
        return CreateRunInput {
            steps: Some(skill_chain_steps(&skill_names)),
            task,
            ..CreateRunInput::default()
        };
    }
    CreateRunInput {
        workflow: Some("quick-task".to_owned()),
        task,
        ..CreateRunInput::default()
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    render_tabs(frame, rows[0], app);

    let Some(data) = app.github_ui.data.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            rows[1],
        );
        return;
    };
    if !data.available {
        render_unavailable(
            frame,
            rows[1],
            data.reason.as_deref().unwrap_or("GitHub is unavailable"),
            app,
        );
        return;
    }

    let list_width = (rows[1].width / 2).clamp(34, 48);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(list_width), Constraint::Min(1)])
        .split(rows[1]);
    render_list(frame, cols[0], app);
    render_detail(frame, cols[1], app);
}

fn render_unavailable(frame: &mut Frame<'_>, area: Rect, reason: &str, app: &mut App) {
    let mut lines = vec![
        Line::from(Span::styled(
            "GitHub is unavailable",
            Style::default()
                .fg(app.theme.palette.fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            reason,
            Style::default().fg(app.theme.palette.soft_fg),
        )),
    ];
    // Only a gh CLI problem warrants the auth guidance; a missing GitHub remote has its own
    // reason and must not read as "unauthenticated".
    if reason.contains("gh CLI")
        || reason.contains("gh not")
        || reason.contains("gh auth")
        || reason.contains("GitHub CLI")
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Install and authenticate the gh CLI (or set GITHUB_TOKEN) to enable this tab.",
            Style::default().fg(app.theme.palette.soft_fg),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let ui = &app.github_ui;
    let count = |tab: GithubTab| match (&ui.data, tab) {
        (Some(data), GithubTab::Issues) => data.issues.len(),
        (Some(data), GithubTab::Prs) => data.prs.len(),
        _ => 0,
    };
    let tabs = [
        (
            GithubTab::Issues,
            format!("Issues · {}", count(GithubTab::Issues)),
        ),
        (
            GithubTab::Prs,
            format!("Pull requests · {}", count(GithubTab::Prs)),
        ),
    ];
    let mut spans = Vec::new();
    let mut x = area.x;
    for (tab, label) in tabs {
        let active = tab == ui.tab;
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
        app.hitmap.register(
            Rect::new(x, area.y, label.chars().count() as u16 + 2, 1),
            3,
            HitAction::GithubScreen(GithubAction::SwitchTab(tab)),
        );
        x = x.saturating_add(label.chars().count() as u16 + 2);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_list(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(match app.github_ui.tab {
            GithubTab::Issues => "Issues",
            GithubTab::Prs => "Pull requests",
        })
        .border_style(if app.screen_focus() == 0 {
            Style::default().fg(app.theme.palette.accent)
        } else {
            Style::default().fg(app.theme.palette.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = items(&app.github_ui);
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing here.").style(Style::default().fg(app.theme.palette.soft_fg)),
            inner,
        );
        return;
    }
    let selected = app
        .github_ui
        .list_selected
        .min(items.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = items
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(index, item)| list_row(item, checks_glyph(item), &app.theme, index == selected))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    for (index, _) in items.iter().enumerate().take(inner.height as usize) {
        if let Some(y) = inner.y.checked_add(index as u16)
            && y < inner.bottom()
        {
            app.hitmap.register(
                Rect::new(inner.x, y, inner.width, 1),
                2,
                HitAction::GithubScreen(GithubAction::SelectItem(index)),
            );
        }
    }
    app.github_ui.list_selected = selected;
}

fn list_row(
    item: &GithubItem,
    glyph: Option<&str>,
    theme: &Theme,
    selected: bool,
) -> Line<'static> {
    let mut style = Style::default().fg(theme.palette.fg);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let mut spans = Vec::new();
    if let Some(glyph) = glyph {
        spans.push(Span::styled(
            format!("{glyph} "),
            Style::default().fg(theme.palette.running),
        ));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        format!("#{}  {}", item.number, item.title),
        style,
    ));
    if item.comments > 0 {
        spans.push(Span::styled(
            format!("  ·{}", item.comments),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    Line::from(spans)
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(item) = app.github_ui.detail_item.clone() else {
        frame.render_widget(
            Paragraph::new("Select an item from the list.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    let sub_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    render_detail_tabs(frame, sub_rows[0], app, &item);
    match app.github_ui.detail_tab {
        GithubDetailTab::Thread => render_thread(frame, sub_rows[1], app, &item),
        GithubDetailTab::Changes => render_changes(frame, sub_rows[1], app),
    }
}

fn render_detail_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App, item: &GithubItem) {
    let mut spans = vec![Span::styled(
        format!("#{}  {}", item.number, item.title),
        Style::default()
            .fg(app.theme.palette.fg)
            .add_modifier(Modifier::BOLD),
    )];
    if !item.labels.is_empty() {
        spans.push(Span::styled(
            format!("   {}", item.labels.join(" · ")),
            Style::default().fg(app.theme.palette.accent),
        ));
    }
    if item.kind == coducktor_contract::GithubItemKind::Pr {
        let mut x = area.x + 40;
        for (tab, label) in [
            (GithubDetailTab::Thread, "Thread"),
            (GithubDetailTab::Changes, "Changes"),
        ] {
            let active = tab == app.github_ui.detail_tab;
            spans.push(Span::styled(
                format!("  {label} "),
                if active {
                    Style::default()
                        .fg(app.theme.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.palette.soft_fg)
                },
            ));
            app.hitmap.register(
                Rect::new(x, area.y, label.chars().count() as u16 + 2, 1),
                3,
                HitAction::GithubScreen(GithubAction::SwitchDetailTab(tab)),
            );
            x = x.saturating_add(label.chars().count() as u16 + 2);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_thread(frame: &mut Frame<'_>, area: Rect, app: &mut App, item: &GithubItem) {
    // Body | (merge gate, PRs only) | comments | hand-to-agent card pinned at the bottom.
    let is_pr = item.kind == coducktor_contract::GithubItemKind::Pr;
    let (body_rows, comment_rows, handoff_row, gate_row) = if is_pr {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),
                Constraint::Length(4),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        (rows[0], rows[2], rows[3], Some(rows[1]))
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        (rows[0], rows[1], rows[2], None)
    };

    let body_block = Block::default().borders(Borders::ALL).title("Body");
    let body_inner = body_block.inner(body_rows);
    frame.render_widget(body_block, body_rows);
    let body = if item.body.trim().is_empty() {
        Paragraph::new("No description.").style(Style::default().fg(app.theme.palette.soft_fg))
    } else {
        Paragraph::new(app.github_ui.markdown.text(&item.body).clone())
    };
    frame.render_widget(body, body_inner);

    if let Some(gate_row) = gate_row {
        render_merge_gate(frame, gate_row, app, item);
    }

    let comment_block = Block::default().borders(Borders::ALL).title("Comments");
    let comment_inner = comment_block.inner(comment_rows);
    frame.render_widget(comment_block, comment_rows);
    match app.github_ui.comments.clone() {
        None => {
            frame.render_widget(
                Paragraph::new("Loading comments…")
                    .style(Style::default().fg(app.theme.palette.soft_fg)),
                comment_inner,
            );
        }
        Some(comments) if !comments.available => {
            frame.render_widget(
                Paragraph::new(
                    comments
                        .reason
                        .unwrap_or_else(|| "Comments are unavailable.".to_owned()),
                )
                .style(Style::default().fg(app.theme.palette.failed)),
                comment_inner,
            );
        }
        Some(comments) if comments.comments.is_empty() => {
            frame.render_widget(
                Paragraph::new("No comments yet.")
                    .style(Style::default().fg(app.theme.palette.soft_fg)),
                comment_inner,
            );
        }
        Some(comments) => {
            let lines: Vec<Line<'static>> = comments
                .comments
                .iter()
                .map(|comment| {
                    Line::from(vec![
                        Span::styled(
                            format!("{} · ", comment.author),
                            Style::default().fg(app.theme.palette.accent),
                        ),
                        Span::styled(
                            comment.body.clone(),
                            Style::default().fg(app.theme.palette.fg),
                        ),
                    ])
                })
                .collect();
            let max_scroll = lines.len().saturating_sub(comment_inner.height as usize);
            app.github_ui.comments_scroll = app.github_ui.comments_scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(lines).scroll((app.github_ui.comments_scroll as u16, 0)),
                comment_inner,
            );
        }
    }

    render_handoff(frame, handoff_row, area, app);
}

fn render_merge_gate(frame: &mut Frame<'_>, area: Rect, app: &mut App, item: &GithubItem) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Merge — #{}", item.number));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.github_ui.merge_state.clone() {
        None => {
            frame.render_widget(
                Paragraph::new("Loading merge state…")
                    .style(Style::default().fg(app.theme.palette.soft_fg)),
                inner,
            );
        }
        Some(coducktor_contract::GithubPrMergeStateResponse::Unavailable { reason, .. }) => {
            frame.render_widget(
                Paragraph::new(format!("Merge is unavailable: {reason}"))
                    .style(Style::default().fg(app.theme.palette.failed)),
                inner,
            );
        }
        Some(coducktor_contract::GithubPrMergeStateResponse::Available { merge_state, .. }) => {
            use coducktor_contract::{GithubMergeable, GithubPrState, GithubReviewDecision};
            let state_label = match merge_state.state {
                GithubPrState::Open => "open",
                GithubPrState::Closed => "closed",
                GithubPrState::Merged => "merged",
            };
            let mergeable_label = match merge_state.mergeable {
                GithubMergeable::Mergeable => "mergeable",
                GithubMergeable::Conflicting => "conflicts",
                GithubMergeable::Unknown => "mergeability unknown",
            };
            let review_label = match merge_state.review_decision {
                GithubReviewDecision::Approved => "approved",
                GithubReviewDecision::ChangesRequested => "changes requested",
                GithubReviewDecision::ReviewRequired => "review required",
                GithubReviewDecision::Unknown => "review unknown",
            };
            let mut lines = vec![Line::from(Span::styled(
                format!("{state_label} · {mergeable_label} · {review_label}"),
                Style::default().fg(app.theme.palette.fg),
            ))];
            for check in &merge_state.checks {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {} {}",
                        match check.state {
                            coducktor_contract::GithubCheckState::Passing => "✓",
                            coducktor_contract::GithubCheckState::Failing => "✗",
                            coducktor_contract::GithubCheckState::Pending => "…",
                            coducktor_contract::GithubCheckState::Unknown => "?",
                        },
                        check.name
                    ),
                    Style::default().fg(app.theme.palette.soft_fg),
                )));
            }
            for blocker in &merge_state.blockers {
                lines.push(Line::from(Span::styled(
                    format!("  ⛔ {}", blocker.message),
                    Style::default().fg(app.theme.palette.failed),
                )));
            }
            lines.push(Line::from(""));
            let method = app.github_ui.merge_method;
            lines.push(Line::from(Span::styled(
                format!(
                    "[m] method: {method}   [enter] merge{}",
                    if merge_state.can_override {
                        " (override rules)"
                    } else {
                        ""
                    }
                ),
                Style::default().fg(app.theme.palette.accent),
            )));
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

fn render_changes(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title("Changes");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match app.github_ui.pr_changes.clone() {
        None => {
            frame.render_widget(
                Paragraph::new("Loading changes…")
                    .style(Style::default().fg(app.theme.palette.soft_fg)),
                inner,
            );
        }
        Some(coducktor_contract::GithubPrChangesData::Unavailable(changes)) => {
            frame.render_widget(
                Paragraph::new(format!("Changes are unavailable: {}", changes.reason))
                    .style(Style::default().fg(app.theme.palette.failed)),
                inner,
            );
        }
        Some(coducktor_contract::GithubPrChangesData::Available(changes)) => {
            let files: Vec<ChangedFile> = changes.files.iter().map(pr_change_to_file).collect();
            let (lines, _) = render_files(
                &files,
                &app.github_ui.changes_diff,
                &app.theme,
                &app.github_ui.highlighter,
                inner.width,
            );
            let max_scroll = lines.len().saturating_sub(inner.height as usize);
            app.github_ui.changes_scroll = app.github_ui.changes_scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(lines).scroll((app.github_ui.changes_scroll as u16, 0)),
                inner,
            );
        }
    }
}

fn pr_change_to_file(change: &coducktor_contract::GithubPrChange) -> ChangedFile {
    ChangedFile {
        path: change.path.clone(),
        old_path: change.previous_path.clone(),
        status: match change.status {
            coducktor_contract::GithubChangeStatus::Added => ChangedFileStatus::Added,
            coducktor_contract::GithubChangeStatus::Modified => ChangedFileStatus::Modified,
            coducktor_contract::GithubChangeStatus::Removed => ChangedFileStatus::Deleted,
            coducktor_contract::GithubChangeStatus::Renamed => ChangedFileStatus::Renamed,
            coducktor_contract::GithubChangeStatus::Copied => ChangedFileStatus::Copied,
            coducktor_contract::GithubChangeStatus::Changed => ChangedFileStatus::Modified,
        },
        adds: change.additions as f64,
        dels: change.deletions as f64,
        binary: change.patch_unavailable_reason
            == Some(coducktor_contract::GithubPatchUnavailableReason::Binary),
        image: None,
        patch: change.patch.clone().unwrap_or_default(),
    }
}

fn render_handoff(frame: &mut Frame<'_>, area: Rect, thread_area: Rect, app: &mut App) {
    let ui = &app.github_ui;
    let mut spans = Vec::new();
    spans.push(Span::styled(
        "Hand this to the agent:  ",
        Style::default()
            .fg(app.theme.palette.fg)
            .add_modifier(Modifier::BOLD),
    ));
    let workflow_label = ui
        .picked_workflow
        .and_then(|index| ui.workflows.get(index))
        .map(|workflow| workflow.name.clone())
        .unwrap_or_else(|| "quick-task".to_owned());
    spans.push(Span::styled(
        format!("[w] workflow: {workflow_label}   "),
        Style::default().fg(app.theme.palette.accent),
    ));
    let skills_label = if ui.picked_skills.is_empty() {
        "none".to_owned()
    } else {
        ui.picked_skills
            .iter()
            .filter_map(|index| ui.skills.get(*index))
            .map(|skill| skill.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    spans.push(Span::styled(
        format!("[s] skills: {skills_label}   "),
        Style::default().fg(app.theme.palette.accent),
    ));
    spans.push(Span::styled(
        "[r] Run agent on this issue",
        Style::default().fg(app.theme.palette.accent),
    ));
    if let Some(queued) = &ui.queued {
        spans.push(Span::styled(
            format!("   ✓ queued — {queued}"),
            Style::default().fg(app.theme.palette.done),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        area,
    );

    if ui.focus == GithubFocus::SkillPicker {
        // The picker overlay over the WHOLE thread pane: filter + ranked skills, `space`
        // toggles, `enter`/`esc` locks the selection back in.
        let width = thread_area.width.min(60);
        let height = (ui.skills.len() + 3).min(16) as u16;
        let dialog = Rect::new(
            thread_area.x + (thread_area.width.saturating_sub(width)) / 2,
            thread_area.y + 1,
            width,
            height,
        );
        let block = Block::default().borders(Borders::ALL).title("Skills");
        let inner = block.inner(dialog);
        frame.render_widget(ratatui::widgets::Clear, dialog);
        frame.render_widget(block, dialog);
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("/ filter: {}", ui.skill_query),
            Style::default().fg(app.theme.palette.soft_fg),
        )));
        let query = ui.skill_query.to_lowercase();
        for (index, skill) in ui.skills.iter().enumerate() {
            if !query.is_empty()
                && !skill.name.to_lowercase().contains(&query)
                && !skill
                    .description
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
            {
                continue;
            }
            let picked = ui.picked_skills.contains(&index);
            lines.push(Line::from(Span::styled(
                format!("{} {}", if picked { "☑" } else { "☐" }, skill.name),
                Style::default().fg(app.theme.palette.fg),
            )));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.github_ui.focus == GithubFocus::SkillPicker {
        return handle_skill_picker_key(app, key);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Tab => {
            let next = match app.github_ui.tab {
                GithubTab::Issues => GithubTab::Prs,
                GithubTab::Prs => GithubTab::Issues,
            };
            apply_hit(app, GithubAction::SwitchTab(next));
            true
        }
        KeyCode::Char('j') | KeyCode::Down if app.github_ui.focus != GithubFocus::Detail => {
            let count = items(&app.github_ui).len();
            if count > 0 {
                app.github_ui.list_selected = (app.github_ui.list_selected + 1).min(count - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up if app.github_ui.focus != GithubFocus::Detail => {
            app.github_ui.list_selected = app.github_ui.list_selected.saturating_sub(1);
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            match app.github_ui.detail_tab {
                GithubDetailTab::Thread => {
                    app.github_ui.comments_scroll = app.github_ui.comments_scroll.saturating_add(1)
                }
                GithubDetailTab::Changes => {
                    app.github_ui.changes_scroll = app.github_ui.changes_scroll.saturating_add(1)
                }
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            match app.github_ui.detail_tab {
                GithubDetailTab::Thread => {
                    app.github_ui.comments_scroll = app.github_ui.comments_scroll.saturating_sub(1)
                }
                GithubDetailTab::Changes => {
                    app.github_ui.changes_scroll = app.github_ui.changes_scroll.saturating_sub(1)
                }
            }
            true
        }
        KeyCode::Enter => {
            let index = app.github_ui.list_selected;
            let detail_matches = app
                .github_ui
                .detail_item
                .as_ref()
                .map(|item| {
                    items(&app.github_ui)
                        .get(index)
                        .map(|listed| listed.number == item.number)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if detail_matches {
                // Enter on the already-open PR is the merge-gate's confirm.
                if app
                    .github_ui
                    .detail_item
                    .as_ref()
                    .is_some_and(|item| item.kind == coducktor_contract::GithubItemKind::Pr)
                {
                    handle_merge_confirm(app);
                }
            } else {
                apply_hit(app, GithubAction::SelectItem(index));
            }
            true
        }
        KeyCode::Char('c') => {
            if app
                .github_ui
                .detail_item
                .as_ref()
                .is_some_and(|item| item.kind == coducktor_contract::GithubItemKind::Pr)
            {
                let next = match app.github_ui.detail_tab {
                    GithubDetailTab::Thread => GithubDetailTab::Changes,
                    GithubDetailTab::Changes => GithubDetailTab::Thread,
                };
                apply_hit(app, GithubAction::SwitchDetailTab(next));
            }
            true
        }
        KeyCode::Char('m') => {
            if app
                .github_ui
                .detail_item
                .as_ref()
                .is_some_and(|item| item.kind == coducktor_contract::GithubItemKind::Pr)
            {
                apply_hit(app, GithubAction::CycleMergeMethod);
            }
            true
        }
        KeyCode::Char('r') => {
            apply_hit(app, GithubAction::RunAgent);
            true
        }
        KeyCode::Char('w') => {
            apply_hit(app, GithubAction::CycleWorkflow);
            true
        }
        KeyCode::Char('s') => {
            if !app.github_ui.skills.is_empty() {
                app.github_ui.focus = GithubFocus::SkillPicker;
                app.github_ui.skill_query.clear();
            }
            true
        }
        KeyCode::Char('R') => {
            let project = app.github_ui.project.clone();
            app.pending.push(PendingAction::LoadGithub { project });
            true
        }
        // External open: a GitHub URL opens in the OS browser without a run round-trip.
        KeyCode::Char('o') => {
            if let Some(url) = items(&app.github_ui)
                .get(app.github_ui.list_selected)
                .map(|item| item.url.clone())
            {
                app.open_url(&url);
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

fn handle_skill_picker_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.github_ui.focus = GithubFocus::List;
            true
        }
        KeyCode::Enter => {
            app.github_ui.focus = GithubFocus::List;
            true
        }
        KeyCode::Char(' ') => {
            // Toggle the first filtered skill (the picker list is short; cycling with
            // j/k + space is the full affordance for this picker.
            let first = app
                .github_ui
                .skills
                .iter()
                .position(|skill| {
                    let query = app.github_ui.skill_query.to_lowercase();
                    query.is_empty() || skill.name.to_lowercase().contains(&query)
                })
                .unwrap_or(0);
            toggle_skill(app, first);
            true
        }
        KeyCode::Backspace => {
            app.github_ui.skill_query.pop();
            true
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.github_ui.skill_query.push(character);
            true
        }
        _ => false,
    }
}

fn toggle_skill(app: &mut App, index: usize) {
    if let Some(position) = app
        .github_ui
        .picked_skills
        .iter()
        .position(|picked| *picked == index)
    {
        app.github_ui.picked_skills.remove(position);
    } else {
        app.github_ui.picked_skills.push(index);
    }
}

pub fn apply_hit(app: &mut App, action: GithubAction) {
    match action {
        GithubAction::SwitchTab(tab) => {
            app.github_ui.tab = tab;
            app.github_ui.list_selected = 0;
            app.github_ui.detail_item = None;
            app.github_ui.comments = None;
            app.github_ui.merge_state = None;
            app.github_ui.pr_changes = None;
        }
        GithubAction::SelectItem(index) => {
            app.github_ui.list_selected = index;
            let Some(item) = items(&app.github_ui).get(index).cloned() else {
                return;
            };
            let number = item.number;
            let kind = match item.kind {
                coducktor_contract::GithubItemKind::Issue => "issue",
                coducktor_contract::GithubItemKind::Pr => "pr",
            };
            let is_pr = item.kind == coducktor_contract::GithubItemKind::Pr;
            let project = app.github_ui.project.clone();
            app.github_ui.detail_item = Some(item);
            app.github_ui.detail_tab = GithubDetailTab::Thread;
            app.github_ui.comments = None;
            app.github_ui.merge_state = None;
            app.github_ui.pr_changes = None;
            app.github_ui.comments_scroll = 0;
            app.github_ui.queued = None;
            app.pending.push(PendingAction::LoadGithubComments {
                project: project.clone(),
                kind: kind.to_owned(),
                number,
            });
            if is_pr {
                app.pending
                    .push(PendingAction::LoadGithubMergeState { project, number });
            }
        }
        GithubAction::SwitchDetailTab(tab) => {
            if app.github_ui.detail_tab == tab {
                return;
            }
            app.github_ui.detail_tab = tab;
            if tab == GithubDetailTab::Changes
                && app.github_ui.pr_changes.is_none()
                && let Some(item) = &app.github_ui.detail_item
            {
                let project = app.github_ui.project.clone();
                app.pending.push(PendingAction::LoadGithubPrChanges {
                    project,
                    number: item.number,
                });
            }
        }
        GithubAction::CycleMergeMethod => {
            app.github_ui.merge_method = match app.github_ui.merge_method {
                GithubMergeMethod::Merge => GithubMergeMethod::Squash,
                GithubMergeMethod::Squash => GithubMergeMethod::Rebase,
                GithubMergeMethod::Rebase => GithubMergeMethod::Merge,
            };
        }
        GithubAction::CycleWorkflow => {
            let count = app.github_ui.workflows.len();
            if count == 0 {
                return;
            }
            app.github_ui.picked_workflow = Some(match app.github_ui.picked_workflow {
                None => 0,
                Some(index) => (index + 1) % count,
            });
        }
        GithubAction::RunAgent => {
            let Some(item) = app.github_ui.detail_item.clone() else {
                return;
            };
            let input = handoff_run_body(&app.github_ui, &item);
            let project = app.github_ui.project.clone();
            app.pending
                .push(PendingAction::GithubHandToAgent { project, input });
        }
    }
}

pub fn handle_merge_confirm(app: &mut App) {
    let Some(item) = app.github_ui.detail_item.clone() else {
        return;
    };
    let Some(coducktor_contract::GithubPrMergeStateResponse::Available { merge_state, .. }) =
        app.github_ui.merge_state.clone()
    else {
        app.notice = Some("merge state is not loaded".to_owned());
        return;
    };
    if !merge_state.can_merge {
        app.notice = Some("this pull request cannot be merged right now".to_owned());
        return;
    }
    let project = app.github_ui.project.clone();
    app.confirm = Some(crate::app::ConfirmRequest {
        text: format!(
            "Merge PR #{} with {}? [y/n]",
            item.number, app.github_ui.merge_method
        ),
        action: PendingAction::GithubMerge {
            project,
            number: item.number,
            method: app.github_ui.merge_method,
            head_sha: merge_state.head_sha.clone(),
            override_rules: merge_state.can_override,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use coducktor_contract::{GithubData, GithubItem, GithubItemKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_item(kind: GithubItemKind, number: u64) -> GithubItem {
        GithubItem {
            kind,
            number,
            title: format!("Fix the thing #{number}"),
            author: "alice".to_owned(),
            created_at: "2026-08-01T00:00:00Z".to_owned(),
            labels: vec!["bug".to_owned()],
            body: "The thing is broken.\n\nSee the URL.".to_owned(),
            url: format!("https://github.com/x/y/issues/{number}"),
            comments: 2,
            is_draft: None,
            additions: None,
            deletions: None,
            checks: None,
        }
    }

    fn app_with_github() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.github_ui.data = Some(GithubData {
            available: true,
            reason: None,
            repo: Some("x/y".to_owned()),
            synced_at: None,
            issues: vec![sample_item(GithubItemKind::Issue, 12)],
            prs: vec![sample_item(GithubItemKind::Pr, 7)],
            label_colors: None,
        });
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
    fn gh_absent_renders_the_reason_and_no_error() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.github_ui.data = Some(GithubData {
            available: false,
            reason: Some("gh not installed or not authenticated".to_owned()),
            repo: None,
            synced_at: None,
            issues: vec![],
            prs: vec![],
            label_colors: None,
        });
        let content = render(&mut app, 120, 40);
        assert!(content.contains("GitHub is unavailable"));
        assert!(content.contains("gh not installed or not authenticated"));
        assert!(content.contains("gh CLI"));
    }

    #[test]
    fn tabs_show_counts_and_list_renders_items() {
        let mut app = app_with_github();
        let content = render(&mut app, 120, 40);
        assert!(content.contains("Issues · 1"));
        assert!(content.contains("Pull requests · 1"));
        assert!(content.contains("#12"));
        assert!(content.contains("Fix the thing #12"));
        apply_hit(&mut app, GithubAction::SwitchTab(GithubTab::Prs));
        let content = render(&mut app, 120, 40);
        assert!(content.contains("#7"));
    }

    #[test]
    fn selecting_an_item_queues_its_detail_reads() {
        let mut app = app_with_github();
        apply_hit(&mut app, GithubAction::SelectItem(0));
        assert_eq!(app.github_ui.detail_item.as_ref().unwrap().number, 12);
        assert!(app.pending.iter().any(|action| {
            matches!(action, PendingAction::LoadGithubComments { kind, number, .. }
                if kind == "issue" && *number == 12)
        }));
        // A PR also loads its merge gate.
        apply_hit(&mut app, GithubAction::SwitchTab(GithubTab::Prs));
        apply_hit(&mut app, GithubAction::SelectItem(0));
        assert!(app.pending.iter().any(|action| {
            matches!(action, PendingAction::LoadGithubMergeState { number, .. } if *number == 7)
        }));
    }

    #[test]
    fn handoff_bodies_follow_the_three_way_rule() {
        let mut app = app_with_github();
        app.github_ui.detail_item = Some(sample_item(GithubItemKind::Issue, 12));
        app.github_ui.workflows = vec![WorkflowDef {
            name: "qa".to_owned(),
            description: None,
            steps: vec![],
            source: coducktor_contract::WorkflowSource::File,
            path: None,
        }];
        app.github_ui.skills = vec![Skill {
            name: "om-fix".to_owned(),
            description: None,
            interactive: None,
            body: "b".to_owned(),
            path: "p".to_owned(),
            source: coducktor_contract::SkillSource::Global,
        }];

        // Nothing picked → quick-task.
        let body = handoff_run_body(&app.github_ui, app.github_ui.detail_item.as_ref().unwrap());
        assert_eq!(body.workflow.as_deref(), Some("quick-task"));
        assert!(body.task.contains("Fix GitHub issue #12"));
        assert!(body.task.contains("https://github.com/x/y/issues/12"));

        // Workflow picked → that workflow.
        app.github_ui.picked_workflow = Some(0);
        let body = handoff_run_body(&app.github_ui, app.github_ui.detail_item.as_ref().unwrap());
        assert_eq!(body.workflow.as_deref(), Some("qa"));
        assert!(body.steps.is_none());

        // Workflow dropped, skill picked → skills ARE the chain.
        app.github_ui.picked_workflow = None;
        app.github_ui.picked_skills = vec![0];
        let body = handoff_run_body(&app.github_ui, app.github_ui.detail_item.as_ref().unwrap());
        assert!(body.workflow.is_none());
        let steps = body.steps.unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].skill.as_deref(), Some("om-fix"));
        assert_eq!(steps[0].prompt.as_deref(), Some("{{task}}"));
        assert_eq!(steps[0].id, "om-fix");
    }

    #[test]
    fn skill_chain_steps_deduplicate_ids() {
        let steps = skill_chain_steps(&[
            "om-fix".to_owned(),
            "om-fix".to_owned(),
            "om-fix".to_owned(),
        ]);
        let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        assert_eq!(ids, vec!["om-fix", "om-fix-2", "om-fix-3"]);
        assert!(steps.len() <= MAX_CHAIN_STEPS);
    }

    #[test]
    fn snapshot_github_at_three_sizes() {
        let mut app = app_with_github();
        apply_hit(&mut app, GithubAction::SelectItem(0));
        app.github_ui.comments = Some(GithubCommentsData {
            available: true,
            reason: None,
            comments: vec![coducktor_contract::GithubComment {
                id: 1,
                author: "bob".to_owned(),
                avatar_url: None,
                created_at: "2026-08-02T00:00:00Z".to_owned(),
                body: "I can repro this locally.".to_owned(),
                kind: coducktor_contract::GithubCommentKind::Comment,
                review_state: None,
                url: "u".to_owned(),
            }],
            truncated: None,
            events: None,
        });
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("github_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
