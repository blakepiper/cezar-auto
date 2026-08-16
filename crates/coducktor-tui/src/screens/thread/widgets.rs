//! Presentational render functions for the task thread's sub-modules (spec §8.4): header +
//! actions, step rail, plan dock, agents dock/subagent sheet, ask card, review panel,
//! auto-resume hint. Each function draws into a given `Rect` and registers its own
//! `HitAction::ThreadScreen(_)` regions; none of them own state beyond what `ThreadUi` passes
//! in. Behavioral spec: `packages/web/src/routes/task-thread/{run-header,step-rail,plan-dock,
//! agents-dock,subagent-sheet,ask-card,review-panel,auto-resume-hint}.tsx`.
//!
//! **Scope note:** the review panel here has no embedded diff — `RunDiff` is spec §8.4's own
//! dependency on the diff engine, which is Phase A's *next* step (A9), not yet built. The
//! banner, notes box and Send back / Draft PR / Accept actions are all present; the diff
//! itself lands when A9's widget exists to embed.

use coducktor_contract::{ApiRun, RunStatus};
use coducktor_protocol::{PlanStatus, UiItem};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::input::hitmap::HitAction;
use crate::screens::runs_util::{attention, run_title};
use crate::theme::Theme;

use super::ThreadAction;
use super::actions::{finish_title, resume_hint, run_action_flags};
use super::reducer::{ThreadAsk, ThreadEntry, ThreadState};

/// The header: title, status pill, meta row, tabs, action bar. Returns the height it used.
pub fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    run: &ApiRun,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    if area.height == 0 {
        return 0;
    }
    let record = &run.record;
    let att = attention(run);
    let mut title_line = vec![
        Span::styled(
            run_title(run),
            Style::default()
                .fg(theme.palette.fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("[{}]", att.label), att.tone.style(theme)),
    ];
    if record.seen_at.is_none() {
        title_line.push(Span::styled(
            " ●",
            Style::default().fg(theme.palette.review),
        ));
    }
    let mut lines = vec![Line::from(title_line)];

    let mut meta = vec![Span::styled(
        format!("{}  ", record.workflow),
        Style::default().fg(theme.palette.soft_fg),
    )];
    if let Some(branch) = &record.branch {
        meta.push(Span::styled(
            format!("{branch}  "),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    if let Some(stat) = &record.diff_stat {
        meta.push(Span::styled(
            format!("+{} -{}  ", stat.adds as i64, stat.dels as i64),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    meta.push(Span::styled(
        format!(
            "{} tok  ${:.2}",
            record.tokens_used as i64,
            record.cost_usd.unwrap_or(0.0)
        ),
        Style::default().fg(theme.palette.soft_fg),
    ));
    lines.push(Line::from(meta));

    let tabs = ["Session", "Changes", "Files", "Commits"];
    let mut tab_spans = Vec::new();
    for (index, tab) in tabs.iter().enumerate() {
        let active = index == 0;
        tab_spans.push(Span::styled(
            format!(" {tab} "),
            if active {
                Style::default()
                    .fg(theme.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.palette.soft_fg)
            },
        ));
    }
    lines.push(Line::from(tab_spans));

    let flags = run_action_flags(run);
    let mut actions: Vec<(&str, ThreadAction)> = Vec::new();
    if flags.finish {
        actions.push(("Finish", ThreadAction::Finish));
    }
    if flags.continue_run {
        actions.push(("Continue", ThreadAction::Continue));
    }
    if flags.terminal {
        actions.push(("Terminal", ThreadAction::Terminal));
    }
    if flags.archive {
        actions.push((
            if record.archived {
                "Restore"
            } else {
                "Archive"
            },
            ThreadAction::Archive,
        ));
    }
    if flags.mark_unread {
        actions.push(("Mark unread", ThreadAction::MarkUnread));
    }
    if flags.cancel {
        actions.push(("Cancel", ThreadAction::Cancel));
    }
    if flags.delete_run {
        actions.push(("Delete", ThreadAction::Delete));
    }
    let mut action_spans = Vec::new();
    for (label, _) in &actions {
        action_spans.push(Span::styled(
            format!("[{label}] "),
            Style::default().fg(theme.palette.fg),
        ));
    }
    lines.push(Line::from(action_spans));

    if let Some(hint) = resume_hint(record) {
        lines.push(Line::from(Span::styled(
            format!("take over: {hint}"),
            Style::default()
                .fg(theme.palette.soft_fg)
                .add_modifier(Modifier::DIM),
        )));
    }

    let height = (lines.len() as u16).min(area.height);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().fg(theme.palette.fg)),
        Rect::new(area.x, area.y, area.width, height),
    );

    // Register action hit-rects on the action-bar row (the 4th line, index 3).
    if let Some(action_row) = area.y.checked_add(3)
        && action_row < area.bottom()
    {
        let mut cursor = area.x;
        for (label, action) in &actions {
            let width = label.chars().count() as u16 + 3;
            hitmap.register(
                Rect::new(cursor, action_row, width, 1),
                4,
                HitAction::ThreadScreen(action.clone()),
            );
            cursor += width;
        }
    }
    height
}

/// The workflow step rail: one collapsed summary line, or the full per-step list when expanded.
pub fn render_step_rail(
    frame: &mut Frame<'_>,
    area: Rect,
    run: &ApiRun,
    collapsed: bool,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    let steps = &run.record.steps;
    if steps.is_empty() || area.height == 0 {
        return 0;
    }
    let total = steps.len();
    let done = steps
        .iter()
        .filter(|s| {
            matches!(
                s.status,
                coducktor_contract::StepStatus::Done
                    | coducktor_contract::StepStatus::Failed
                    | coducktor_contract::StepStatus::Cancelled
                    | coducktor_contract::StepStatus::Skipped
            )
        })
        .count();
    let active_index = steps
        .iter()
        .position(|s| {
            matches!(
                s.status,
                coducktor_contract::StepStatus::Running
                    | coducktor_contract::StepStatus::Waiting
                    | coducktor_contract::StepStatus::Review
            )
        })
        .or_else(|| {
            steps
                .iter()
                .position(|s| s.status == coducktor_contract::StepStatus::Pending)
        })
        .unwrap_or(total.saturating_sub(1));
    let current_name = steps
        .get(active_index)
        .map(|s| s.name.as_str())
        .unwrap_or("");
    let line = Line::from(vec![
        Span::styled(
            if collapsed { "\u{25b8} " } else { "\u{25be} " },
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled(
            current_name.to_owned(),
            Style::default().fg(theme.palette.fg),
        ),
        Span::styled(
            format!("  step {} of {total}", active_index + 1),
            Style::default().fg(theme.palette.soft_fg),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hitmap.register(
        Rect::new(area.x, area.y, area.width, 1),
        4,
        HitAction::ThreadScreen(ThreadAction::ToggleStepRail),
    );
    if collapsed || area.height < 2 {
        return 1;
    }
    let mut row = area.y + 1;
    let bottom = area.bottom();
    for step in steps {
        if row >= bottom {
            break;
        }
        let glyph = match step.status {
            coducktor_contract::StepStatus::Done => {
                Span::styled("✓ ", Style::default().fg(theme.palette.done))
            }
            coducktor_contract::StepStatus::Failed => {
                Span::styled("✗ ", Style::default().fg(theme.palette.failed))
            }
            coducktor_contract::StepStatus::Cancelled | coducktor_contract::StepStatus::Skipped => {
                Span::styled("- ", Style::default().fg(theme.palette.soft_fg))
            }
            coducktor_contract::StepStatus::Running
            | coducktor_contract::StepStatus::Waiting
            | coducktor_contract::StepStatus::Review => {
                Span::styled("● ", Style::default().fg(theme.palette.running))
            }
            coducktor_contract::StepStatus::Pending => {
                Span::styled("○ ", Style::default().fg(theme.palette.soft_fg))
            }
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                glyph,
                Span::styled(step.name.clone(), Style::default().fg(theme.palette.fg)),
            ])),
            Rect::new(area.x + 2, row, area.width.saturating_sub(2), 1),
        );
        row += 1;
    }
    let _ = done;
    row - area.y
}

/// The `plan.updated` checklist strip — the latest snapshot across the whole thread.
pub fn render_plan_dock(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &ThreadState,
    collapsed: bool,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    let Some(entries) = super::reducer::latest_plan_entries(state) else {
        return 0;
    };
    if entries.is_empty() || area.height == 0 {
        return 0;
    }
    let done = entries
        .iter()
        .filter(|e| e.status == PlanStatus::Completed)
        .count();
    let total = entries
        .iter()
        .filter(|e| e.status != PlanStatus::Cancelled)
        .count();
    let active = entries
        .iter()
        .find(|e| e.status == PlanStatus::InProgress)
        .or_else(|| entries.iter().find(|e| e.status == PlanStatus::Pending));
    let header = Line::from(vec![
        Span::styled(
            if collapsed { "\u{25b8} " } else { "\u{25be} " },
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled("PLAN ", Style::default().fg(theme.palette.accent)),
        Span::styled(
            format!("{done}/{total}"),
            Style::default().fg(theme.palette.soft_fg),
        ),
        active
            .map(|entry| {
                Span::styled(
                    format!("  {}", entry.content),
                    Style::default().fg(theme.palette.fg),
                )
            })
            .unwrap_or_default(),
    ]);
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hitmap.register(
        Rect::new(area.x, area.y, area.width, 1),
        4,
        HitAction::ThreadScreen(ThreadAction::TogglePlanDock),
    );
    if collapsed || area.height < 2 {
        return 1;
    }
    let mut row = area.y + 1;
    for entry in entries {
        if row >= area.bottom() {
            break;
        }
        let glyph = match entry.status {
            PlanStatus::Completed => Span::styled("✓ ", Style::default().fg(theme.palette.done)),
            PlanStatus::InProgress => {
                Span::styled("◐ ", Style::default().fg(theme.palette.waiting))
            }
            PlanStatus::Pending => Span::styled("○ ", Style::default().fg(theme.palette.soft_fg)),
            PlanStatus::Cancelled => Span::styled("⊘ ", Style::default().fg(theme.palette.soft_fg)),
        };
        let text_style = if entry.status == PlanStatus::Cancelled {
            Style::default()
                .fg(theme.palette.soft_fg)
                .add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default().fg(theme.palette.fg)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                glyph,
                Span::styled(entry.content.clone(), text_style),
            ])),
            Rect::new(area.x + 2, row, area.width.saturating_sub(2), 1),
        );
        row += 1;
    }
    row - area.y
}

/// The sub-agent fan-out strip. Defaults collapsed, unlike the plan dock.
pub fn render_agents_dock(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &ThreadState,
    collapsed: bool,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    let subagents = collect_subagents(state);
    if subagents.is_empty() || area.height == 0 {
        return 0;
    }
    let done = subagents
        .iter()
        .filter(|item| item.status == coducktor_protocol::ToolStatus::Completed)
        .count();
    let header = Line::from(vec![
        Span::styled(
            if collapsed { "\u{25b8} " } else { "\u{25be} " },
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled("AGENTS ", Style::default().fg(theme.palette.accent)),
        Span::styled(
            format!("{done}/{}", subagents.len()),
            Style::default().fg(theme.palette.soft_fg),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hitmap.register(
        Rect::new(area.x, area.y, area.width, 1),
        4,
        HitAction::ThreadScreen(ThreadAction::ToggleAgentsDock),
    );
    if collapsed || area.height < 2 {
        return 1;
    }
    let mut row = area.y + 1;
    for item in &subagents {
        if row >= area.bottom() {
            break;
        }
        let status_span = match item.status {
            coducktor_protocol::ToolStatus::Completed => {
                Span::styled("✓ ", Style::default().fg(theme.palette.done))
            }
            coducktor_protocol::ToolStatus::Failed | coducktor_protocol::ToolStatus::Declined => {
                Span::styled("✗ ", Style::default().fg(theme.palette.failed))
            }
            _ => Span::styled("● ", Style::default().fg(theme.palette.running)),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                status_span,
                Span::styled(item.title.clone(), Style::default().fg(theme.palette.fg)),
            ])),
            Rect::new(area.x + 2, row, area.width.saturating_sub(2), 1),
        );
        hitmap.register(
            Rect::new(area.x, row, area.width, 1),
            4,
            HitAction::ThreadScreen(ThreadAction::OpenSubagent(item.id.clone())),
        );
        row += 1;
    }
    row - area.y
}

/// A parentless `toolKind:task` item is a sub-agent — anchored on the latest turn carrying
/// root task items (spec §8.4 `subagent-dock.ts::collectSubagents`, simplified: this port
/// scans every turn rather than bounding to the most recent unsettled fan-out, which only
/// matters for very long-running multi-turn subagent chains).
fn collect_subagents(state: &ThreadState) -> Vec<coducktor_protocol::UiToolItem> {
    let mut out = Vec::new();
    for turn in &state.turns {
        for entry in &turn.items {
            if let ThreadEntry::Item(UiItem::Tool(tool)) = entry
                && tool.tool_kind == coducktor_protocol::ToolKind::Task
                && tool.parent_item_id.is_none()
            {
                out.push(tool.clone());
            }
        }
    }
    out
}

/// The AskUser card: the agent asked one or more structured questions via `CEZ:ASK`.
pub fn render_ask_card(
    frame: &mut Frame<'_>,
    area: Rect,
    ask: &ThreadAsk,
    selections: &[Vec<String>],
    focus: (usize, usize),
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    if area.height == 0 {
        return 0;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "The agent is asking",
            Style::default()
                .fg(theme.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let mut row = area.y + 1;
    for (qi, question) in ask.questions.iter().enumerate() {
        if row >= area.bottom() {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", question.header),
                    Style::default().fg(theme.palette.accent),
                ),
                Span::styled(
                    question.question.clone(),
                    Style::default()
                        .fg(theme.palette.fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            Rect::new(area.x, row, area.width, 1),
        );
        row += 1;
        for (oi, option) in question.options.iter().enumerate() {
            if row >= area.bottom() {
                break;
            }
            let selected = selections
                .get(qi)
                .is_some_and(|labels| labels.contains(&option.label));
            let is_focus = focus == (qi, oi);
            let marker = if selected { "[x]" } else { "[ ]" };
            let style = if is_focus {
                Style::default()
                    .fg(theme.palette.bg)
                    .bg(theme.palette.accent)
            } else if selected {
                Style::default().fg(theme.palette.accent)
            } else {
                Style::default().fg(theme.palette.fg)
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  {marker} {}", option.label),
                    style,
                ))),
                Rect::new(area.x, row, area.width, 1),
            );
            hitmap.register(
                Rect::new(area.x, row, area.width, 1),
                4,
                HitAction::ThreadScreen(ThreadAction::AskOption {
                    question: qi,
                    option: oi,
                }),
            );
            row += 1;
        }
    }
    if row < area.bottom() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " [Send answer]  Space select · Enter send",
                Style::default().fg(theme.palette.soft_fg),
            ))),
            Rect::new(area.x, row, area.width, 1),
        );
        hitmap.register(
            Rect::new(area.x, row, 14, 1),
            4,
            HitAction::ThreadScreen(ThreadAction::AskSend),
        );
        row += 1;
    }
    row - area.y
}

/// The review gate: a violet banner, notes box, and Send back / Draft PR / Accept.
pub fn render_review_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    run: &ApiRun,
    notes_preview: &str,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    if area.height == 0 {
        return 0;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Review the changes before anything lands.",
            Style::default()
                .fg(theme.palette.review)
                .add_modifier(Modifier::BOLD),
        )))
        .wrap(Wrap { trim: false }),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let notes_row = area.y + 1;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("notes: ", Style::default().fg(theme.palette.soft_fg)),
            Span::raw(notes_preview.to_owned()),
        ])),
        Rect::new(area.x, notes_row, area.width, 1),
    );
    let actions_row = area.y + 2;
    let mut actions = vec![("Send back", ThreadAction::ReviewSendBack)];
    if let Some(url) = run
        .record
        .pull_request_url
        .as_deref()
        .filter(|url| url.starts_with("http"))
    {
        let _ = url;
        actions.push(("PR ↗", ThreadAction::ReviewOpenPr));
    } else {
        actions.push(("Draft PR", ThreadAction::ReviewDraftPr));
    }
    actions.push(("Accept", ThreadAction::ReviewAccept));
    let mut cursor = area.x;
    let mut cursor_line = Vec::new();
    for (label, action) in &actions {
        let width = label.chars().count() as u16 + 3;
        cursor_line.push(Span::styled(
            format!("[{label}] "),
            Style::default().fg(theme.palette.fg),
        ));
        if actions_row < area.bottom() {
            hitmap.register(
                Rect::new(cursor, actions_row, width, 1),
                4,
                HitAction::ThreadScreen(action.clone()),
            );
        }
        cursor += width;
    }
    if actions_row < area.bottom() {
        frame.render_widget(
            Paragraph::new(Line::from(cursor_line)),
            Rect::new(area.x, actions_row, area.width, 1),
        );
    }
    let _ = finish_title(RunStatus::Review);
    3.min(area.height)
}

/// The `failed` + `autoResumeAt` hint: the absolute resume deadline and a "Don't resume" exit.
pub fn render_auto_resume_hint(
    frame: &mut Frame<'_>,
    area: Rect,
    run: &ApiRun,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) -> u16 {
    if area.height == 0 {
        return 0;
    }
    let Some(at) = run.record.auto_resume_at.as_deref() else {
        return 0;
    };
    let line = Line::from(vec![
        Span::styled(
            format!("scheduled to resume at {at} "),
            Style::default().fg(theme.palette.waiting),
        ),
        Span::styled("[Don't resume]", Style::default().fg(theme.palette.fg)),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hitmap.register(
        Rect::new(area.x, area.y, area.width, 1),
        4,
        HitAction::ThreadScreen(ThreadAction::CancelAutoResume),
    );
    1
}

/// The one-line hint under the dock for a paused/waiting or a queued run.
pub fn render_status_hint(frame: &mut Frame<'_>, area: Rect, text: &str, theme: &Theme) -> u16 {
    if area.height == 0 || text.is_empty() {
        return 0;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            text.to_owned(),
            Style::default().fg(theme.palette.soft_fg),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    1
}

/// The right-side sub-agent drill-down panel: the full item stream for one agent.
pub fn render_subagent_sheet(
    frame: &mut Frame<'_>,
    area: Rect,
    agent: &coducktor_protocol::UiToolItem,
    state: &ThreadState,
    theme: &Theme,
    hitmap: &mut crate::input::hitmap::HitMap,
) {
    use ratatui::widgets::Clear;
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", agent.title))
        .border_style(Style::default().fg(theme.palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    hitmap.register(
        area,
        6,
        HitAction::ThreadScreen(ThreadAction::CloseSubagentSheet),
    );

    let mut lines = Vec::new();
    for turn in &state.turns {
        for entry in &turn.items {
            if let ThreadEntry::Item(item) = entry
                && item_parent(item) == Some(agent.id.as_str())
            {
                lines.push(render_child_line(item, theme));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no activity recorded yet)",
            Style::default().fg(theme.palette.soft_fg),
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

fn item_parent(item: &UiItem) -> Option<&str> {
    match item {
        UiItem::Message(item) => item.parent_item_id.as_deref(),
        UiItem::Reasoning(item) => item.parent_item_id.as_deref(),
        UiItem::Tool(item) => item.parent_item_id.as_deref(),
    }
}

fn render_child_line(item: &UiItem, theme: &Theme) -> Line<'static> {
    match item {
        UiItem::Message(m) => Line::from(Span::styled(
            m.text.clone(),
            Style::default().fg(theme.palette.fg),
        )),
        UiItem::Reasoning(r) => Line::from(Span::styled(
            format!("(thinking) {}", r.text.lines().next().unwrap_or_default()),
            Style::default().fg(theme.palette.soft_fg),
        )),
        UiItem::Tool(t) => Line::from(Span::styled(
            t.title.clone(),
            Style::default().fg(theme.palette.fg),
        )),
    }
}

pub(super) fn queue_hint(is_queued: bool) -> &'static str {
    if is_queued {
        "Messages you add now are folded into the prompt before the run starts."
    } else {
        ""
    }
}
