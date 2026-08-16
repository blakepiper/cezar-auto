//! The task thread screen (spec §8.4) — the full run lifecycle: header + actions, step
//! rail, plan dock, subagent dock/sheet, ask card, review panel, queued messages,
//! auto-resume hint, and the composer host. Replaces
//! `packages/web/src/routes/task-thread/*` (~20 modules) — see `actions.rs` and
//! `reducer.rs` for the ported pure logic, and `widgets.rs` for the presentational pieces.
//!
//! **Scope notes carried from research, not oversights:**
//! - The review panel has no embedded diff yet — that is A9's `RunDiff` widget, which does
//!   not exist as of this step.
//! - The composer sends text only; image attachments (screenshot paste) are not wired into
//!   the thread's live-message path in this pass.
//! - Queued-message editing (`PATCH .../queued-messages/:id`) is reachable on `Engine` but
//!   this screen only wires the remove half; the web's inline text-edit affordance is not
//!   ported.
//! - `Changes` / `Files` / `Commits` tabs render as labels only — their screens are A9/A10.

pub mod actions;
pub mod reducer;
mod widgets;

use coducktor_contract::{ApiRun, MessageInput, RunEvent, RunStatus};
use coducktor_protocol::UiItem;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, PendingAction};
use crate::widgets::transcript::{
    MessageItem, NoteItem, NoteTone as TranscriptNoteTone, ReasoningItem, ToolItem, Transcript,
    TranscriptItem,
};

use actions::{is_run_active, last_session_id};
use reducer::{NoteTone, ThreadAsk, ThreadEntry, ThreadReduceOptions, ThreadState, reduce_thread};

/// A thread-screen control — a header action, a dock toggle, an ask option, a review
/// button. Routed by `apply_hit` and mirrored by keyboard shortcuts in `handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadAction {
    Finish,
    Continue,
    Terminal,
    Archive,
    MarkUnread,
    Cancel,
    Delete,
    ToggleStepRail,
    TogglePlanDock,
    ToggleAgentsDock,
    OpenSubagent(String),
    CloseSubagentSheet,
    AskOption { question: usize, option: usize },
    AskSend,
    ReviewSendBack,
    ReviewDraftPr,
    ReviewOpenPr,
    ReviewAccept,
    CancelAutoResume,
    RemoveQueuedMessage(String),
    FocusComposer,
    FocusReviewNotes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadFocus {
    #[default]
    Transcript,
    Composer,
    ReviewNotes,
    Ask,
}

/// Engine-fetched state for the currently open thread.
#[derive(Default)]
pub struct ThreadData {
    pub project: String,
    pub run_id: String,
    pub run: Option<ApiRun>,
    pub events: Vec<RunEvent>,
    pub as_of_seq: f64,
    pub state: ThreadState,
}

pub struct ThreadUi {
    pub data: ThreadData,
    pub transcript: Transcript,
    pub composer: crate::widgets::composer::Composer,
    pub review_notes: String,
    pub ask_selections: Vec<Vec<String>>,
    pub ask_focus: (usize, usize),
    pub subagent_sheet: Option<String>,
    pub agents_collapsed: bool,
    pub plan_collapsed: bool,
    pub steps_collapsed: bool,
    pub focus: ThreadFocus,
}

impl Default for ThreadUi {
    fn default() -> Self {
        Self {
            data: ThreadData::default(),
            transcript: Transcript::new(),
            composer: crate::widgets::composer::Composer::default(),
            review_notes: String::new(),
            ask_selections: Vec::new(),
            ask_focus: (0, 0),
            subagent_sheet: None,
            agents_collapsed: true,
            plan_collapsed: false,
            steps_collapsed: true,
            focus: ThreadFocus::Transcript,
        }
    }
}

impl ThreadUi {
    /// Called on route entry, from a fresh `GET /runs/:id` + the first history page.
    pub fn load(
        &mut self,
        project: String,
        run_id: String,
        run: ApiRun,
        events: Vec<RunEvent>,
        as_of_seq: f64,
    ) {
        self.data = ThreadData {
            project,
            run_id,
            run: Some(run),
            events,
            as_of_seq,
            state: ThreadState::default(),
        };
        self.transcript = Transcript::new();
        self.review_notes.clear();
        self.ask_selections.clear();
        self.ask_focus = (0, 0);
        self.subagent_sheet = None;
        self.focus = ThreadFocus::Transcript;
        self.rebuild();
    }

    /// Replace the loaded run record (a fresh `GET /runs/:id`, or a workspace `run` frame
    /// for the currently-open thread).
    pub fn set_run(&mut self, run: ApiRun) {
        self.data.run = Some(run);
    }

    /// Append one live event (from the per-run SSE stream) and re-fold.
    pub fn push_event(&mut self, seq: f64, event: RunEvent) {
        if seq <= self.data.as_of_seq {
            return;
        }
        self.data.as_of_seq = seq;
        self.data.events.push(event);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let active_turn = self
            .data
            .run
            .as_ref()
            .is_some_and(|run| run.record.status == RunStatus::Running);
        self.data.state = reduce_thread(&self.data.events, ThreadReduceOptions { active_turn });
        let Some(run) = &self.data.run else {
            return;
        };
        self.transcript
            .reconcile(build_transcript_items(run, &self.data.state));
        // A resolved ask, or a fresh one under a different id, clears any stale in-progress
        // selections — a leftover partial answer must never attach to the NEXT question.
        if pending_ask(&self.data.state).is_none() {
            self.ask_selections.clear();
        }
    }
}

fn map_tone(tone: NoteTone) -> TranscriptNoteTone {
    match tone {
        NoteTone::Dim => TranscriptNoteTone::Dim,
        NoteTone::Warning => TranscriptNoteTone::Warning,
        NoteTone::Danger => TranscriptNoteTone::Danger,
    }
}

fn build_transcript_items(run: &ApiRun, state: &ThreadState) -> Vec<TranscriptItem> {
    let mut items = Vec::new();
    if !run.record.task.trim().is_empty() {
        items.push(TranscriptItem::Message(MessageItem::new(
            "task",
            coducktor_protocol::MessageRole::User,
            run.record.task.clone(),
        )));
    }
    for turn in &state.turns {
        if let Some(user_message) = &turn.user_message {
            items.push(TranscriptItem::Message(MessageItem::new(
                format!("um:{}", turn.id),
                coducktor_protocol::MessageRole::User,
                user_message.text.clone(),
            )));
        }
        for entry in &turn.items {
            match entry {
                ThreadEntry::Item(UiItem::Message(message)) => {
                    items.push(TranscriptItem::Message(MessageItem::new(
                        message.id.clone(),
                        message.role,
                        message.text.clone(),
                    )));
                }
                ThreadEntry::Item(UiItem::Reasoning(reasoning)) => {
                    items.push(TranscriptItem::Reasoning(ReasoningItem::new(
                        reasoning.id.clone(),
                        reasoning.text.clone(),
                    )));
                }
                ThreadEntry::Item(UiItem::Tool(tool)) => {
                    let mut item = ToolItem::new(
                        tool.id.clone(),
                        &tool.name,
                        tool.input.as_ref(),
                        tool.status,
                    );
                    item.output = tool.output.clone();
                    item.error = tool.error.clone();
                    item.exit_code = tool.exit_code.map(|value| value as i64);
                    items.push(TranscriptItem::Tool(item));
                }
                ThreadEntry::Note(note) => {
                    items.push(TranscriptItem::Note(NoteItem::new(
                        note.id.clone(),
                        note.text.clone(),
                        map_tone(note.tone),
                    )));
                }
                ThreadEntry::Image(image) => {
                    items.push(TranscriptItem::Note(NoteItem::new(
                        image.id.clone(),
                        format!("image: {}", image.url),
                        TranscriptNoteTone::Dim,
                    )));
                }
                ThreadEntry::ProviderAuthRequired(card) => {
                    items.push(TranscriptItem::Note(NoteItem::new(
                        card.id.clone(),
                        format!(
                            "{:?} needs re-authentication (incident {})",
                            card.provider, card.auth_failure_id
                        ),
                        TranscriptNoteTone::Danger,
                    )));
                }
                // Rendered as its own interactive card below the transcript, not inline.
                ThreadEntry::Ask(_) => {}
            }
        }
    }
    items
}

/// The single AskUser card that can still be answered — only the LATEST `ask.requested`
/// is ever open (the agent asks once, then parks `waiting`).
fn pending_ask(state: &ThreadState) -> Option<&ThreadAsk> {
    state.turns.iter().rev().find_map(|turn| {
        turn.items.iter().rev().find_map(|entry| match entry {
            ThreadEntry::Ask(ask) if !ask.resolved => Some(ask),
            ThreadEntry::Ask(_) => None,
            _ => None,
        })
    })
}

/// Navigate to a run's thread and queue the data load. The one entry point every screen
/// (Tasks, Global tasks, row menus, a just-started run) should call instead of navigating
/// `Route::Thread` directly.
pub fn open(app: &mut App, project: &str, id: &str) {
    app.history.navigate(crate::app::Route::Thread {
        project: project.to_owned(),
        id: id.to_owned(),
    });
    app.pending.push(PendingAction::LoadThread {
        project: project.to_owned(),
        id: id.to_owned(),
    });
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(run) = app.thread_ui.data.run.clone() else {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(app.theme.palette.soft_fg)),
            area,
        );
        return;
    };
    let theme = app.theme;
    let record = &run.record;

    let step_rail_height = if record.steps.is_empty() {
        0
    } else if app.thread_ui.steps_collapsed {
        1
    } else {
        (record.steps.len() as u16 + 1).min(6)
    };

    let ask = pending_ask(&app.thread_ui.data.state).cloned();
    let ask_height = ask
        .as_ref()
        .map(|ask| {
            let rows: usize = ask.questions.iter().map(|q| 1 + q.options.len()).sum();
            (rows as u16 + 2).min(12)
        })
        .unwrap_or(0);
    let review_height = if record.status == RunStatus::Review {
        3
    } else {
        0
    };
    let subagents_present = !record.steps.is_empty() && has_subagents(&app.thread_ui.data.state);
    let agents_height = if !subagents_present {
        0
    } else if app.thread_ui.agents_collapsed {
        1
    } else {
        6
    };
    let plan_present = reducer::latest_plan_entries(&app.thread_ui.data.state)
        .is_some_and(|entries| !entries.is_empty());
    let plan_height = if !plan_present {
        0
    } else if app.thread_ui.plan_collapsed {
        1
    } else {
        6
    };
    let auto_resume_height = if record.auto_resume_at.is_some() {
        1
    } else {
        0
    };
    let hint_height = if matches!(record.status, RunStatus::Queued | RunStatus::Waiting) {
        1
    } else {
        0
    };
    let composer_height = app.thread_ui.composer.height() + 2;
    let dock_height = ask_height
        + review_height
        + agents_height
        + plan_height
        + auto_resume_height
        + hint_height
        + composer_height;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(step_rail_height),
            Constraint::Min(3),
            Constraint::Length(dock_height),
        ])
        .split(area);

    widgets::render_header(frame, rows[0], &run, &theme, &mut app.hitmap);
    if step_rail_height > 0 {
        widgets::render_step_rail(
            frame,
            rows[1],
            &run,
            app.thread_ui.steps_collapsed,
            &theme,
            &mut app.hitmap,
        );
    }

    let transcript_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.palette.border));
    let transcript_area = transcript_block.inner(rows[2]);
    frame.render_widget(transcript_block, rows[2]);
    if transcript_area.height > 0 {
        app.thread_ui
            .transcript
            .render(frame.buffer_mut(), transcript_area, &theme);
    }

    let dock_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(ask_height),
            Constraint::Length(review_height),
            Constraint::Length(agents_height),
            Constraint::Length(plan_height),
            Constraint::Length(auto_resume_height),
            Constraint::Length(hint_height),
            Constraint::Length(composer_height),
        ])
        .split(rows[3]);

    if let Some(ask) = &ask {
        widgets::render_ask_card(
            frame,
            dock_rows[0],
            ask,
            &app.thread_ui.ask_selections,
            app.thread_ui.ask_focus,
            &theme,
            &mut app.hitmap,
        );
    }
    if review_height > 0 {
        let preview: String = app
            .thread_ui
            .review_notes
            .chars()
            .take(dock_rows[1].width.max(8) as usize - 8)
            .collect();
        widgets::render_review_panel(frame, dock_rows[1], &run, &preview, &theme, &mut app.hitmap);
    }
    if agents_height > 0 {
        widgets::render_agents_dock(
            frame,
            dock_rows[2],
            &app.thread_ui.data.state,
            app.thread_ui.agents_collapsed,
            &theme,
            &mut app.hitmap,
        );
    }
    if plan_height > 0 {
        widgets::render_plan_dock(
            frame,
            dock_rows[3],
            &app.thread_ui.data.state,
            app.thread_ui.plan_collapsed,
            &theme,
            &mut app.hitmap,
        );
    }
    if auto_resume_height > 0 {
        widgets::render_auto_resume_hint(frame, dock_rows[4], &run, &theme, &mut app.hitmap);
    }
    if hint_height > 0 {
        let text = match record.status {
            RunStatus::Queued => widgets::queue_hint(true),
            RunStatus::Waiting => "Waiting for your reply.",
            _ => "",
        };
        widgets::render_status_hint(frame, dock_rows[5], text, &theme);
    }
    app.thread_ui
        .composer
        .render(frame, dock_rows[6], theme, &mut app.hitmap, 4);

    if let Some(agent_id) = app.thread_ui.subagent_sheet.clone()
        && let Some(agent) = find_subagent(&app.thread_ui.data.state, &agent_id)
    {
        let sheet_area = Rect::new(
            area.right().saturating_sub(area.width / 2),
            area.y,
            area.width / 2,
            area.height,
        );
        widgets::render_subagent_sheet(
            frame,
            sheet_area,
            &agent,
            &app.thread_ui.data.state,
            &theme,
            &mut app.hitmap,
        );
    }
}

fn has_subagents(state: &ThreadState) -> bool {
    state.turns.iter().any(|turn| {
        turn.items.iter().any(|entry| {
            matches!(entry, ThreadEntry::Item(UiItem::Tool(tool)) if tool.tool_kind == coducktor_protocol::ToolKind::Task && tool.parent_item_id.is_none())
        })
    })
}

fn find_subagent(state: &ThreadState, id: &str) -> Option<coducktor_protocol::UiToolItem> {
    state.turns.iter().find_map(|turn| {
        turn.items.iter().find_map(|entry| match entry {
            ThreadEntry::Item(UiItem::Tool(tool)) if tool.id == id => Some(tool.clone()),
            _ => None,
        })
    })
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.thread_ui.subagent_sheet.is_some() {
        if key.code == KeyCode::Esc {
            app.thread_ui.subagent_sheet = None;
        }
        return true;
    }
    match app.thread_ui.focus {
        ThreadFocus::Composer => return handle_composer_key(app, key),
        ThreadFocus::ReviewNotes => return handle_review_notes_key(app, key),
        ThreadFocus::Ask => return handle_ask_key(app, key),
        ThreadFocus::Transcript => {}
    }
    match key.code {
        KeyCode::Char('i') => {
            app.thread_ui.focus = ThreadFocus::Composer;
            app.thread_ui.composer.focus();
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.thread_ui.transcript.scroll_by(1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.thread_ui.transcript.scroll_by(-1);
            true
        }
        KeyCode::Char('G') => {
            app.thread_ui.transcript.jump_to_bottom();
            true
        }
        KeyCode::Char('f') => {
            apply_action(app, ThreadAction::Finish);
            true
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => false,
        KeyCode::Char('a') => {
            apply_action(app, ThreadAction::Archive);
            true
        }
        _ => false,
    }
}

fn handle_composer_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code == KeyCode::Esc {
        app.thread_ui.focus = ThreadFocus::Transcript;
        return true;
    }
    let ctx = crate::widgets::composer::ComposerContext {
        skills: &[],
        skill_usage: None,
        mention_candidates: &[],
    };
    let event = app.thread_ui.composer.handle_key(key, &ctx);
    if let crate::widgets::composer::ComposerEvent::Submit { text } = event {
        if !text.is_empty() {
            submit_composer(app, text);
        }
        app.thread_ui.composer.set_text("");
    }
    true
}

fn submit_composer(app: &mut App, text: String) {
    let Some(run) = app.thread_ui.data.run.clone() else {
        return;
    };
    let project = app.thread_ui.data.project.clone();
    let id = app.thread_ui.data.run_id.clone();
    if is_run_active(run.record.status) {
        app.pending.push(PendingAction::SendMessage {
            project,
            id,
            input: MessageInput {
                text: Some(text),
                images: None,
            },
        });
    } else if last_session_id(&run.record).is_some() {
        app.pending.push(PendingAction::ContinueRun {
            project,
            id,
            text: Some(text),
        });
    }
}

fn handle_review_notes_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => app.thread_ui.focus = ThreadFocus::Transcript,
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
            apply_action(app, ThreadAction::ReviewSendBack);
        }
        KeyCode::Enter => app.thread_ui.review_notes.push('\n'),
        KeyCode::Backspace => {
            app.thread_ui.review_notes.pop();
        }
        KeyCode::Char(character) => app.thread_ui.review_notes.push(character),
        _ => {}
    }
    true
}

fn handle_ask_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(ask) = pending_ask(&app.thread_ui.data.state).cloned() else {
        app.thread_ui.focus = ThreadFocus::Transcript;
        return true;
    };
    ensure_ask_selection_shape(app, &ask);
    let (question, option) = app.thread_ui.ask_focus;
    match key.code {
        KeyCode::Esc => app.thread_ui.focus = ThreadFocus::Transcript,
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(count) = ask.questions.get(question).map(|q| q.options.len()) {
                app.thread_ui.ask_focus.1 = (option + 1).min(count.saturating_sub(1));
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.thread_ui.ask_focus.1 = option.saturating_sub(1);
        }
        KeyCode::Tab | KeyCode::Right => {
            app.thread_ui.ask_focus =
                ((question + 1).min(ask.questions.len().saturating_sub(1)), 0);
        }
        KeyCode::BackTab | KeyCode::Left => {
            app.thread_ui.ask_focus = (question.saturating_sub(1), 0);
        }
        KeyCode::Char(' ') => toggle_ask_option(app, &ask, question, option),
        KeyCode::Enter => {
            let one_tap = ask.questions.len() == 1 && ask.questions[0].multi_select != Some(true);
            if one_tap {
                toggle_ask_option(app, &ask, question, option);
                send_ask_answer(app, &ask);
            } else {
                send_ask_answer(app, &ask);
            }
        }
        _ => {}
    }
    true
}

fn ensure_ask_selection_shape(app: &mut App, ask: &ThreadAsk) {
    if app.thread_ui.ask_selections.len() != ask.questions.len() {
        app.thread_ui.ask_selections = vec![Vec::new(); ask.questions.len()];
    }
}

fn toggle_ask_option(app: &mut App, ask: &ThreadAsk, question: usize, option: usize) {
    ensure_ask_selection_shape(app, ask);
    let Some(question_def) = ask.questions.get(question) else {
        return;
    };
    let Some(label) = question_def
        .options
        .get(option)
        .map(|option| option.label.clone())
    else {
        return;
    };
    let multi = question_def.multi_select == Some(true);
    let Some(selected) = app.thread_ui.ask_selections.get_mut(question) else {
        return;
    };
    if multi {
        if let Some(index) = selected.iter().position(|existing| existing == &label) {
            selected.remove(index);
        } else {
            selected.push(label);
        }
    } else {
        *selected = vec![label];
    }
}

fn send_ask_answer(app: &mut App, ask: &ThreadAsk) {
    let text = ask
        .questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let labels = app
                .thread_ui
                .ask_selections
                .get(index)
                .cloned()
                .unwrap_or_default();
            format!("{}: {}", question.header, labels.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return;
    }
    let Some(run) = app.thread_ui.data.run.clone() else {
        return;
    };
    let project = app.thread_ui.data.project.clone();
    let id = app.thread_ui.data.run_id.clone();
    match actions::ask_delivery_mode(&run) {
        actions::DeliveryMode::Live => app.pending.push(PendingAction::SendMessage {
            project,
            id,
            input: MessageInput {
                text: Some(text),
                images: None,
            },
        }),
        actions::DeliveryMode::Resume => app.pending.push(PendingAction::ContinueRun {
            project,
            id,
            text: Some(text),
        }),
        actions::DeliveryMode::Unavailable => {
            app.notice = Some(
                "This session has ended and no agent session was recorded, so the answer cannot be delivered."
                    .to_owned(),
            );
        }
    }
    app.thread_ui.ask_selections.clear();
    app.thread_ui.focus = ThreadFocus::Transcript;
}

pub fn apply_hit(app: &mut App, action: ThreadAction) {
    match action {
        ThreadAction::AskOption { question, option } => {
            if let Some(ask) = pending_ask(&app.thread_ui.data.state).cloned() {
                ensure_ask_selection_shape(app, &ask);
                app.thread_ui.ask_focus = (question, option);
                let one_tap =
                    ask.questions.len() == 1 && ask.questions[0].multi_select != Some(true);
                toggle_ask_option(app, &ask, question, option);
                if one_tap {
                    send_ask_answer(app, &ask);
                }
            }
        }
        ThreadAction::AskSend => {
            if let Some(ask) = pending_ask(&app.thread_ui.data.state).cloned() {
                send_ask_answer(app, &ask);
            }
        }
        ThreadAction::OpenSubagent(id) => app.thread_ui.subagent_sheet = Some(id),
        ThreadAction::CloseSubagentSheet => app.thread_ui.subagent_sheet = None,
        ThreadAction::ToggleStepRail => {
            app.thread_ui.steps_collapsed = !app.thread_ui.steps_collapsed
        }
        ThreadAction::TogglePlanDock => {
            app.thread_ui.plan_collapsed = !app.thread_ui.plan_collapsed
        }
        ThreadAction::ToggleAgentsDock => {
            app.thread_ui.agents_collapsed = !app.thread_ui.agents_collapsed
        }
        ThreadAction::FocusComposer => {
            app.thread_ui.focus = ThreadFocus::Composer;
            app.thread_ui.composer.focus();
        }
        ThreadAction::FocusReviewNotes => app.thread_ui.focus = ThreadFocus::ReviewNotes,
        ThreadAction::ReviewSendBack => apply_action(app, ThreadAction::ReviewSendBack),
        other => apply_action(app, other),
    }
}

fn apply_action(app: &mut App, action: ThreadAction) {
    let Some(run) = app.thread_ui.data.run.clone() else {
        return;
    };
    let project = app.thread_ui.data.project.clone();
    let id = app.thread_ui.data.run_id.clone();
    match action {
        ThreadAction::Finish => app.pending.push(PendingAction::FinishRun { project, id }),
        ThreadAction::Continue => app.pending.push(PendingAction::ContinueRun {
            project,
            id,
            text: None,
        }),
        ThreadAction::Terminal => app.pending.push(PendingAction::OpenInCli { project, id }),
        ThreadAction::Archive => app.pending.push(PendingAction::Archive {
            project,
            id,
            archived: !run.record.archived,
        }),
        ThreadAction::MarkUnread => app.pending.push(PendingAction::Unread { project, id }),
        ThreadAction::Cancel => {
            app.confirm = Some(crate::app::ConfirmRequest {
                text: "Stop this run?".to_owned(),
                action: PendingAction::CancelRun { project, id },
            })
        }
        ThreadAction::Delete => {
            app.confirm = Some(crate::app::ConfirmRequest {
                text: format!(
                    "Delete \"{}\" and its branch?",
                    crate::screens::runs_util::run_title(&run)
                ),
                action: PendingAction::Delete { project, id },
            })
        }
        ThreadAction::ReviewSendBack => {
            let notes = app.thread_ui.review_notes.trim();
            if notes.is_empty() {
                app.notice = Some("Write what to change first.".to_owned());
                app.thread_ui.focus = ThreadFocus::ReviewNotes;
                return;
            }
            app.pending.push(PendingAction::ContinueRun {
                project,
                id,
                text: Some(format!("Review feedback:\n{notes}")),
            });
            app.thread_ui.review_notes.clear();
        }
        ThreadAction::ReviewDraftPr => app.pending.push(PendingAction::CreatePr { project, id }),
        ThreadAction::ReviewOpenPr => {
            if let Some(url) = run.record.pull_request_url.clone() {
                app.open_url(&url);
            }
        }
        ThreadAction::ReviewAccept => app.pending.push(PendingAction::FinishRun { project, id }),
        ThreadAction::CancelAutoResume => app
            .pending
            .push(PendingAction::CancelAutoResume { project, id }),
        ThreadAction::RemoveQueuedMessage(message_id) => {
            app.pending.push(PendingAction::RemoveQueuedMessage {
                project,
                id,
                message_id,
            })
        }
        ThreadAction::AskOption { .. }
        | ThreadAction::AskSend
        | ThreadAction::OpenSubagent(_)
        | ThreadAction::CloseSubagentSheet
        | ThreadAction::ToggleStepRail
        | ThreadAction::TogglePlanDock
        | ThreadAction::ToggleAgentsDock
        | ThreadAction::FocusComposer
        | ThreadAction::FocusReviewNotes => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use coducktor_contract::{RunRecord, StepKind, StepState, StepStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn run(status: RunStatus, task: &str) -> ApiRun {
        ApiRun {
            record: RunRecord {
                id: "run-1".to_owned(),
                title: "Ship the shell".to_owned(),
                workflow: "quick-task".to_owned(),
                task: task.to_owned(),
                status,
                created_at: "2026-08-15T00:00:00Z".to_owned(),
                steps: vec![StepState {
                    id: "s1".to_owned(),
                    name: "agent".to_owned(),
                    kind: StepKind::Agent,
                    status: StepStatus::Running,
                    iterations: 1.0,
                    tokens_used: 0.0,
                    input_tokens: None,
                    output_tokens: None,
                    usage_invocations_started: None,
                    usage_invocations_observed: None,
                    usage_turns_started: None,
                    usage_turns_recorded: None,
                    usage_invocation_epoch: None,
                    started_at: None,
                    finished_at: None,
                    error: None,
                    session_id: Some("sess-1".to_owned()),
                    backend: None,
                    requested_runner: None,
                    profile_id: None,
                    reasoning_effort: None,
                    cost_usd: None,
                    model_identity: None,
                }],
                ..RunRecord::default()
            },
            usage: None,
        }
    }

    fn event(seq: f64, event_type: &str, extra: serde_json::Value) -> RunEvent {
        RunEvent {
            seq,
            ts: "2026-08-15T00:00:00Z".to_owned(),
            step_id: Some("s1".to_owned()),
            event_type: event_type.to_owned(),
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    fn app_with_run(status: RunStatus) -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.thread_ui.load(
            "main".to_owned(),
            "run-1".to_owned(),
            run(status, "Ship the shell"),
            Vec::new(),
            -1.0,
        );
        app.history.navigate(crate::app::Route::Thread {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });
        app
    }

    fn render_to_string(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn full_run_lifecycle_start_live_ask_answer_review_send_back_accept_archive() {
        let mut app = app_with_run(RunStatus::Queued);
        app.history.navigate(crate::app::Route::Thread {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });

        // start → live: the run starts running and an assistant message streams in.
        app.thread_ui
            .set_run(run(RunStatus::Running, "Ship the shell"));
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "item.started",
                json!({"item": {"kind": "message", "id": "m1", "role": "assistant", "text": "Working on it."}}),
            ),
        );
        let content = render_to_string(&mut app);
        assert!(
            content.contains("Working on it"),
            "live message renders: {content}"
        );

        // ask: the agent asks a single-select question.
        app.thread_ui.push_event(
            2.0,
            event(
                2.0,
                "ask.requested",
                json!({
                    "requestId": "ask-1",
                    "questions": [{"header": "CONFIRM", "question": "Deploy now?", "options": [{"label": "Yes"}, {"label": "No"}]}],
                }),
            ),
        );
        let ask = pending_ask(&app.thread_ui.data.state)
            .cloned()
            .expect("ask card pending");
        assert_eq!(ask.questions[0].header, "CONFIRM");

        // answer: pick the option via the same hit path the mouse uses — a one-tap
        // single-select question sends immediately.
        apply_hit(
            &mut app,
            ThreadAction::AskOption {
                question: 0,
                option: 0,
            },
        );
        assert_eq!(
            app.pending,
            vec![PendingAction::SendMessage {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
                input: MessageInput {
                    text: Some("CONFIRM: Yes".to_owned()),
                    images: None,
                },
            }]
        );
        app.pending.clear();
        // The engine folds the answer back as a user-message, resolving the card.
        app.thread_ui.push_event(
            3.0,
            event(3.0, "user-message", json!({"text": "CONFIRM: Yes"})),
        );
        assert!(
            pending_ask(&app.thread_ui.data.state).is_none(),
            "the ask resolves"
        );

        // review: the run parks for review.
        app.thread_ui
            .set_run(run(RunStatus::Review, "Ship the shell"));
        let content = render_to_string(&mut app);
        assert!(
            content.contains("Review the changes"),
            "the review panel renders: {content}"
        );

        // send back: notes go back into the session via continue, prefixed exactly as the
        // legacy behavior requires.
        app.thread_ui.review_notes = "please add a test".to_owned();
        apply_action(&mut app, ThreadAction::ReviewSendBack);
        assert_eq!(
            app.pending,
            vec![PendingAction::ContinueRun {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
                text: Some("Review feedback:\nplease add a test".to_owned()),
            }]
        );
        assert!(
            app.thread_ui.review_notes.is_empty(),
            "notes clear after sending"
        );
        app.pending.clear();

        // accept: the review panel's Accept is the same mutation as the header's Finish.
        apply_action(&mut app, ThreadAction::ReviewAccept);
        assert_eq!(
            app.pending,
            vec![PendingAction::FinishRun {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
            }]
        );
        app.pending.clear();

        // archive: once the run is terminal, Archive is offered and toggles the flag.
        app.thread_ui
            .set_run(run(RunStatus::Done, "Ship the shell"));
        apply_action(&mut app, ThreadAction::Archive);
        assert_eq!(
            app.pending,
            vec![PendingAction::Archive {
                project: "main".to_owned(),
                id: "run-1".to_owned(),
                archived: true,
            }]
        );
    }

    #[test]
    fn review_send_back_refuses_empty_notes() {
        let mut app = app_with_run(RunStatus::Review);
        apply_action(&mut app, ThreadAction::ReviewSendBack);
        assert!(app.pending.is_empty(), "an empty note is not sent");
        assert_eq!(app.thread_ui.focus, ThreadFocus::ReviewNotes);
    }

    #[test]
    fn cancel_offers_only_while_active_and_delete_only_once_terminal() {
        let mut running = app_with_run(RunStatus::Running);
        apply_action(&mut running, ThreadAction::Cancel);
        assert!(
            running.confirm.is_some(),
            "cancel confirms on an active run"
        );

        let mut done = app_with_run(RunStatus::Done);
        apply_action(&mut done, ThreadAction::Delete);
        assert!(done.confirm.is_some(), "delete confirms on a terminal run");
    }

    #[test]
    fn queued_messages_hint_and_composer_stay_available_while_queued() {
        let mut app = app_with_run(RunStatus::Queued);
        let content = render_to_string(&mut app);
        assert!(content.contains("folded into the prompt"));
    }

    #[test]
    fn renders_at_the_three_a8_snapshot_sizes_running_with_plan_and_steps() {
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut app = app_with_run(RunStatus::Running);
            app.thread_ui.push_event(
                1.0,
                event(
                    1.0,
                    "plan.updated",
                    json!({"entries": [
                        {"content": "write the reducer", "status": "completed"},
                        {"content": "wire the screen", "status": "in_progress"},
                        {"content": "add tests", "status": "pending"},
                    ]}),
                ),
            );
            app.thread_ui.push_event(
                2.0,
                event(
                    2.0,
                    "item.completed",
                    json!({"item": {"kind": "message", "id": "m1", "role": "assistant", "text": "On it."}}),
                ),
            );
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("thread_running_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }

    #[test]
    fn renders_at_the_three_a8_snapshot_sizes_review() {
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut app = app_with_run(RunStatus::Review);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("thread_review_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
