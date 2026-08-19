//! The task thread screen: the full run lifecycle, including actions, steps, plans, subagents,
//! questions, review, queued messages, auto-resume, and composition. Pure policies live in
//! `actions.rs` and `reducer.rs`; rendering helpers live in `widgets.rs`.
//!
//! The review panel exposes its actions without embedding a full diff. The composer sends text,
//! and the task Git tabs provide Changes, Files, and Commits navigation.

use std::path::PathBuf;

pub mod actions;
pub mod presenters;
pub mod projection;
pub mod reducer;
mod widgets;

use coducktor_contract::{ApiRun, ImageInput, MessageInput, RunEvent, RunStatus};
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
use projection::ThreadViewModel;
use reducer::{NoteTone, ThreadAsk, ThreadEntry, ThreadReduceOptions, ThreadState, reduce_thread};

/// A thread-screen control — a header action, an ask option, a review
/// button. Routed by `apply_hit` and mirrored by keyboard shortcuts in `handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadAction {
    ToggleTimelineItem(usize),
    Finish,
    Continue,
    Terminal,
    Archive,
    MarkUnread,
    Cancel,
    Delete,
    ToggleStepRail,
    OpenSubagent(String),
    CloseSubagentSheet,
    AskOption {
        question: usize,
        option: usize,
    },
    AskSend,
    ReviewSendBack,
    ReviewDraftPr,
    ReviewOpenPr,
    ReviewAccept,
    CancelAutoResume,
    RemoveQueuedMessage(String),
    FocusComposer,
    FocusReviewNotes,
    /// Tab row: Session is this screen; Changes/Files/Commits are `screens::task_git` — leaving
    /// this screen is a navigation, not a local state change.
    OpenGitTab(crate::app::TaskGitTab),
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
    pub older_cursor: Option<String>,
    pub older_loading: bool,
    pub older_error: Option<String>,
    pub state: ThreadState,
    pub view_model: ThreadViewModel,
}

pub struct ThreadUi {
    pub data: ThreadData,
    pub transcript: Transcript,
    pub composer: crate::widgets::composer::Composer,
    pub review_notes: String,
    pub ask_selections: Vec<Vec<String>>,
    pub ask_focus: (usize, usize),
    pub subagent_sheet: Option<String>,
    pub steps_collapsed: bool,
    pub focus: ThreadFocus,
    /// The transcript's inner rectangle from the last render, used to keep mouse-wheel input
    /// scoped to the task activity rather than the composer and other controls.
    pub(crate) transcript_area: Option<Rect>,
    pub pending_prompt: Option<String>,
    pending_prompt_after_seq: f64,
    pending_composer: Option<crate::widgets::composer::Composer>,
    pub delivery_error: bool,
    pub cancel_pending: bool,
    pub project_root: Option<PathBuf>,
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
            steps_collapsed: true,
            focus: ThreadFocus::Transcript,
            transcript_area: None,
            pending_prompt: None,
            pending_prompt_after_seq: -1.0,
            pending_composer: None,
            delivery_error: false,
            cancel_pending: false,
            project_root: None,
        }
    }
}

impl ThreadUi {
    /// Called on thread entry, from a fresh run record and the first history page.
    pub fn load(
        &mut self,
        project: String,
        run_id: String,
        run: ApiRun,
        events: Vec<RunEvent>,
        as_of_seq: f64,
        older_cursor: Option<String>,
    ) {
        let same_thread = self.data.project == project && self.data.run_id == run_id;
        self.data = ThreadData {
            project,
            run_id,
            run: Some(run),
            events,
            as_of_seq,
            older_cursor,
            older_loading: false,
            older_error: None,
            state: ThreadState::default(),
            view_model: ThreadViewModel::default(),
        };
        self.transcript = Transcript::new();
        self.review_notes.clear();
        self.ask_selections.clear();
        self.ask_focus = (0, 0);
        self.subagent_sheet = None;
        self.focus = ThreadFocus::Transcript;
        self.transcript_area = None;
        if !same_thread {
            self.pending_prompt = None;
            self.pending_prompt_after_seq = -1.0;
            self.pending_composer = None;
        }
        self.cancel_pending = false;
        self.rebuild();
    }

    pub fn set_pending_prompt(&mut self, text: String) {
        self.pending_prompt = Some(text);
        self.pending_prompt_after_seq = self.data.as_of_seq;
        self.pending_composer = None;
        self.delivery_error = false;
        self.rebuild();
    }

    fn set_pending_composer(&mut self, text: String) {
        self.pending_prompt = Some(text);
        self.pending_prompt_after_seq = self.data.as_of_seq;
        self.pending_composer = Some(self.composer.clone());
        self.delivery_error = false;
        self.rebuild();
    }

    pub fn set_project_root(&mut self, root: Option<PathBuf>) {
        self.project_root = root;
        self.rebuild();
    }

    pub fn restore_pending_prompt(&mut self, project: &str, run_id: &str) {
        if self.data.project != project || self.data.run_id != run_id {
            return;
        }
        if let Some(composer) = self.pending_composer.take() {
            self.composer = composer;
            self.composer.focus();
            self.focus = ThreadFocus::Composer;
        } else if let Some(text) = &self.pending_prompt {
            self.composer.set_text(text);
            self.composer.focus();
            self.focus = ThreadFocus::Composer;
        }
        self.pending_prompt = None;
        self.pending_prompt_after_seq = -1.0;
        self.delivery_error = true;
        self.rebuild();
    }

    /// Replace the loaded run record (a fresh engine read or a workspace run event
    /// for the currently-open thread).
    pub fn set_run(&mut self, run: ApiRun) {
        self.data.run = Some(run);
        self.rebuild();
    }

    pub fn clear_if_matches(&mut self, project: &str, run_id: &str) {
        if self.data.project != project || self.data.run_id != run_id {
            return;
        }
        self.data.run = None;
        self.data.events.clear();
        self.data.state = ThreadState::default();
        self.data.view_model = ThreadViewModel::default();
        self.data.as_of_seq = 0.0;
        self.data.older_cursor = None;
        self.data.older_loading = false;
        self.data.older_error = None;
        self.transcript = Transcript::new();
        self.pending_prompt = None;
        self.pending_prompt_after_seq = -1.0;
        self.pending_composer = None;
        self.delivery_error = false;
    }

    /// Append one live run event and re-fold.
    pub fn push_event(&mut self, seq: f64, event: RunEvent) {
        self.push_events(std::iter::once((seq, event)));
    }

    /// Append a frame-sized live batch and re-fold once. This keeps projection cost linear in
    /// frames rather than rebuilding the complete transcript for every provider delta.
    pub fn push_events(&mut self, events: impl IntoIterator<Item = (f64, RunEvent)>) {
        let mut changed = false;
        for (seq, event) in events {
            if seq <= self.data.as_of_seq {
                continue;
            }
            self.data.as_of_seq = seq;
            self.data.events.push(event);
            changed = true;
        }
        if changed {
            self.rebuild();
        }
    }

    pub fn begin_load_earlier(&mut self) -> Option<String> {
        if self.data.older_loading {
            return None;
        }
        let cursor = self.data.older_cursor.clone()?;
        self.data.older_loading = true;
        self.data.older_error = None;
        Some(cursor)
    }

    pub fn merge_earlier(&mut self, events: Vec<RunEvent>, older_cursor: Option<String>) {
        let old_len = self.data.events.len();
        self.data.events.extend(events);
        self.data.events.sort_by(|a, b| a.seq.total_cmp(&b.seq));
        self.data.events.dedup_by(|a, b| a.seq == b.seq);
        let added = self.data.events.len().saturating_sub(old_len);
        self.data.older_cursor = older_cursor;
        self.data.older_loading = false;
        self.data.older_error = None;
        self.rebuild();
        self.transcript.preserve_after_prepend(added);
    }

    pub fn fail_load_earlier(&mut self, error: String) {
        self.data.older_loading = false;
        self.data.older_error = Some(error);
    }

    fn rebuild(&mut self) {
        // The send request being accepted does not mean its user-message has reached the
        // transcript yet. Keep the optimistic prompt visible until the durable event arrives,
        // then let that event replace it without briefly hiding what the agent is working on.
        let pending_is_durable = self.pending_prompt.as_deref().is_some_and(|prompt| {
            self.data.events.iter().any(|event| {
                event.seq > self.pending_prompt_after_seq
                    && event.event_type == "user-message"
                    && durable_prompt_label(event) == prompt
            })
        });
        if pending_is_durable {
            self.pending_prompt = None;
            self.pending_prompt_after_seq = -1.0;
            self.pending_composer = None;
            self.delivery_error = false;
        }
        let active_turn = self
            .data
            .run
            .as_ref()
            .is_some_and(|run| run.record.status == RunStatus::Running);
        self.data.state = reduce_thread(&self.data.events, ThreadReduceOptions { active_turn });
        let Some(run) = &self.data.run else {
            return;
        };
        self.data.view_model = projection::project_thread_with_root(
            run,
            &self.data.state,
            self.project_root.as_deref(),
        );
        self.transcript.reconcile(build_transcript_items(
            run,
            &self.data.state,
            &self.data.view_model,
            self.data.older_cursor.is_some(),
            self.data.older_loading,
            self.data.older_error.as_deref(),
            self.pending_prompt.as_deref(),
        ));
        // A resolved ask, or a fresh one under a different id, clears any stale in-progress
        // selections — a leftover partial answer must never attach to the NEXT question.
        if pending_ask(&self.data.state).is_none() {
            self.ask_selections.clear();
        }
    }
}

fn durable_prompt_label(event: &RunEvent) -> &str {
    let text = event
        .extra
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if text.is_empty()
        && event
            .extra
            .get("imageCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
            > 0
    {
        "[Image]"
    } else {
        text
    }
}

fn map_tone(tone: NoteTone) -> TranscriptNoteTone {
    match tone {
        NoteTone::Dim => TranscriptNoteTone::Dim,
        NoteTone::Warning => TranscriptNoteTone::Warning,
        NoteTone::Danger => TranscriptNoteTone::Danger,
    }
}

fn build_transcript_items(
    run: &ApiRun,
    state: &ThreadState,
    view_model: &ThreadViewModel,
    has_older: bool,
    older_loading: bool,
    older_error: Option<&str>,
    pending_prompt: Option<&str>,
) -> Vec<TranscriptItem> {
    let mut items = Vec::new();
    if older_loading {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-loading",
            "Loading earlier history…",
            TranscriptNoteTone::Dim,
        )));
    } else if let Some(error) = older_error {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-error",
            format!("Earlier history failed: {error} — press R to retry"),
            TranscriptNoteTone::Danger,
        )));
    } else if has_older {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-earlier",
            "Earlier history available — scroll to the top or press R to load",
            TranscriptNoteTone::Dim,
        )));
    }
    if !run.record.task.trim().is_empty() {
        items.push(TranscriptItem::Message(MessageItem::new(
            "task",
            coducktor_protocol::MessageRole::User,
            run.record.task.clone(),
        )));
    }
    for (turn_index, turn) in state.turns.iter().enumerate() {
        let projected = view_model
            .turns
            .iter()
            .find(|item| item.id == turn.id)
            .or_else(|| {
                (turn_index == 0 && turn.user_message.is_none())
                    .then(|| view_model.turns.first())
                    .flatten()
            });
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
                    let is_final = projected
                        .and_then(|turn| turn.response.as_ref())
                        .is_some_and(|response| response.id == message.id);
                    if message.role == coducktor_protocol::MessageRole::Assistant && !is_final {
                        items.push(TranscriptItem::Note(NoteItem::new(
                            message.id.clone(),
                            message.text.clone(),
                            TranscriptNoteTone::Dim,
                        )));
                    } else {
                        items.push(TranscriptItem::Message(MessageItem::new(
                            message.id.clone(),
                            message.role,
                            message.text.clone(),
                        )));
                    }
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
        if let Some(plan) = &turn.plan_entries {
            let completed = plan
                .iter()
                .filter(|entry| entry.status == coducktor_protocol::PlanStatus::Completed)
                .count();
            items.push(TranscriptItem::Note(NoteItem::new(
                format!("plan:{}", turn.id),
                format!("Plan {completed}/{} complete", plan.len()),
                TranscriptNoteTone::Dim,
            )));
        }
        if let Some(projected) = projected
            && let Some(outcome) = outcome_item(projected)
        {
            items.push(outcome);
        }
    }
    if let Some(prompt) = pending_prompt {
        items.push(TranscriptItem::Message(MessageItem::new(
            "pending-prompt",
            coducktor_protocol::MessageRole::User,
            prompt,
        )));
        items.push(TranscriptItem::Note(NoteItem::new(
            "pending-prompt-state",
            "Sending…",
            TranscriptNoteTone::Dim,
        )));
    }
    items
}

fn outcome_item(turn: &projection::TurnViewModel) -> Option<TranscriptItem> {
    let (text, tone) = match &turn.outcome {
        projection::TurnOutcome::Running | projection::TurnOutcome::Unknown => return None,
        projection::TurnOutcome::Completed { verification, .. } => (
            format!(
                "Completed · verification {}",
                verification_label(*verification)
            ),
            TranscriptNoteTone::Dim,
        ),
        projection::TurnOutcome::Failed {
            reason,
            verification,
        } => (
            format!(
                "Failed{} · verification {}",
                reason
                    .as_deref()
                    .map(|value| format!(": {value}"))
                    .unwrap_or_default(),
                verification_label(*verification)
            ),
            TranscriptNoteTone::Danger,
        ),
        projection::TurnOutcome::Interrupted => {
            ("Interrupted".to_owned(), TranscriptNoteTone::Warning)
        }
    };
    Some(TranscriptItem::Note(NoteItem::new(
        format!("outcome:{}", turn.id),
        text,
        tone,
    )))
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
    let root = app
        .project_registry
        .iter()
        .find(|entry| entry.id == project)
        .map(|entry| PathBuf::from(&entry.root))
        .or_else(|| {
            (app.default_project == project)
                .then(|| app.boot_root.clone())
                .flatten()
        });
    app.thread_ui.set_project_root(root);
    if (app.thread_ui.data.project != project || app.thread_ui.data.run_id != id)
        && let Some(run) = app
            .project_tasks
            .get(project)
            .and_then(|state| state.runs.iter().find(|run| run.record.id == id))
            .cloned()
    {
        app.thread_ui.load(
            project.to_owned(),
            id.to_owned(),
            run,
            Vec::new(),
            -1.0,
            None,
        );
    }
    app.navigate_route(crate::app::Route::Thread {
        project: project.to_owned(),
        id: id.to_owned(),
    });
    app.pending.push(PendingAction::LoadThread {
        project: project.to_owned(),
        id: id.to_owned(),
    });
}

/// Open a newly accepted run without a round trip through the manager. Agent execution may hold
/// that manager for the duration of a turn, so the create response is the authoritative first
/// frame: it contains the exact prompt, queued status, and durable run id needed by the listener.
pub fn open_started(app: &mut App, project: &str, run: coducktor_contract::RunRecord) {
    let id = run.id.clone();
    let root = app
        .project_registry
        .iter()
        .find(|entry| entry.id == project)
        .map(|entry| PathBuf::from(&entry.root))
        .or_else(|| {
            (app.default_project == project)
                .then(|| app.boot_root.clone())
                .flatten()
        });
    app.thread_ui.set_project_root(root);
    app.thread_ui.load(
        project.to_owned(),
        id.clone(),
        ApiRun {
            record: run,
            usage: None,
        },
        Vec::new(),
        -1.0,
        None,
    );
    app.navigate_route(crate::app::Route::Thread {
        project: project.to_owned(),
        id,
    });
}

fn request_earlier(app: &mut App) {
    let Some(cursor) = app.thread_ui.begin_load_earlier() else {
        return;
    };
    app.pending.push(PendingAction::LoadEarlierThread {
        project: app.thread_ui.data.project.clone(),
        id: app.thread_ui.data.run_id.clone(),
        cursor,
    });
}

/// Scroll the task transcript in response to a wheel event. A wheel notch moves three
/// transcript rows, matching the usual terminal mouse-wheel behavior while preserving the
/// transcript's existing bounds and follow-mode handling.
pub fn handle_scroll(app: &mut App, up: bool) {
    if up && app.thread_ui.transcript.at_top() {
        request_earlier(app);
    }
    app.thread_ui.transcript.scroll_by(if up { -3 } else { 3 });
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
    let auto_resume_height = if record.auto_resume_at.is_some() {
        1
    } else {
        0
    };
    let hint_height = 1;
    let composer_height = app
        .thread_ui
        .composer
        .height_for_width(area.width.saturating_sub(3))
        + 2;
    let dock_height =
        ask_height + review_height + auto_resume_height + hint_height + composer_height;

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

    const THROBBER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let active = matches!(record.status, RunStatus::Queued | RunStatus::Running);
    let throbber = THROBBER[(app.animation_tick as usize / 3) % THROBBER.len()];
    let status = if app.thread_ui.cancel_pending {
        format!("{throbber} Stopping…")
    } else if active {
        format!(
            "{throbber} {}",
            app.thread_ui.data.view_model.current_status
        )
    } else {
        app.thread_ui.data.view_model.current_status.clone()
    };
    let transcript_title = format!(
        " Session  ·  {}{} ",
        status,
        if app.thread_ui.transcript.unseen_count() > 0 {
            format!("  ·  {} new", app.thread_ui.transcript.unseen_count())
        } else {
            String::new()
        }
    );
    let transcript_block = Block::default()
        .title(transcript_title)
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.palette.border));
    let transcript_area = transcript_block.inner(rows[2]);
    app.thread_ui.transcript_area = Some(transcript_area);
    frame.render_widget(transcript_block, rows[2]);
    if transcript_area.height > 0 {
        app.thread_ui.transcript.render_interactive(
            frame.buffer_mut(),
            transcript_area,
            &theme,
            &mut app.hitmap,
        );
    }

    let dock_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(ask_height),
            Constraint::Length(review_height),
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
    if auto_resume_height > 0 {
        widgets::render_auto_resume_hint(frame, dock_rows[2], &run, &theme, &mut app.hitmap);
    }
    if hint_height > 0 {
        let text = match record.status {
            RunStatus::Queued if app.thread_ui.cancel_pending => "Stopping the agent…",
            RunStatus::Queued => widgets::queue_hint(true),
            RunStatus::Waiting => "Waiting for your reply.",
            RunStatus::Running if app.thread_ui.cancel_pending => "Stopping the agent…",
            RunStatus::Running => "Agent is working · Enter sends guidance to the active turn.",
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => {
                "Enter starts a follow-up turn."
            }
            RunStatus::Review => "Enter starts a follow-up; review actions are above.",
        };
        widgets::render_status_hint(frame, dock_rows[3], text, &theme);
    }
    app.thread_ui
        .composer
        .set_title(if app.thread_ui.pending_prompt.is_some() {
            "SENDING"
        } else if app.thread_ui.delivery_error {
            "SEND FAILED · RETRY"
        } else {
            match record.status {
                RunStatus::Queued => "QUEUED",
                RunStatus::Waiting => "ANSWER",
                RunStatus::Running => "GUIDANCE",
                RunStatus::Review | RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => {
                    if last_session_id(record).is_some() {
                        "FOLLOW UP"
                    } else {
                        "NEW TASK"
                    }
                }
            }
        });
    app.thread_ui
        .composer
        .render(frame, dock_rows[4], theme, &mut app.hitmap, 4);

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

fn verification_label(status: projection::VerificationStatus) -> &'static str {
    match status {
        projection::VerificationStatus::Passed => "passed",
        projection::VerificationStatus::Failed => "failed",
        projection::VerificationStatus::NotObserved => "not observed",
    }
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
            if app.thread_ui.transcript.at_top() {
                request_earlier(app);
            }
            app.thread_ui.transcript.scroll_by(-1);
            true
        }
        KeyCode::Char('G') => {
            app.thread_ui.transcript.jump_to_bottom();
            true
        }
        KeyCode::Tab => {
            app.thread_ui.transcript.select_next_expandable(1);
            true
        }
        KeyCode::BackTab => {
            app.thread_ui.transcript.select_next_expandable(-1);
            true
        }
        KeyCode::Enter => {
            app.thread_ui.transcript.toggle_selected();
            true
        }
        KeyCode::Char('R') => {
            request_earlier(app);
            true
        }
        KeyCode::Char('f') => {
            apply_action(app, ThreadAction::Finish);
            true
        }
        KeyCode::Char(']') => {
            apply_hit(
                app,
                ThreadAction::OpenGitTab(crate::app::TaskGitTab::Changes),
            );
            true
        }
        KeyCode::Char('[') => {
            apply_hit(
                app,
                ThreadAction::OpenGitTab(crate::app::TaskGitTab::Commits),
            );
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

/// Deliver a bracketed-paste chunk to the focused composer.
pub fn handle_paste(app: &mut App, text: &str) -> bool {
    if app.thread_ui.subagent_sheet.is_some() || app.thread_ui.focus != ThreadFocus::Composer {
        return false;
    }
    // Image pastes have no textual payload in some terminal implementations. In that case,
    // read the native clipboard so a copied image can still become a follow-up attachment.
    if text.is_empty() {
        return handle_clipboard_paste(app);
    }
    let ctx = crate::widgets::composer::ComposerContext {
        skills: &[],
        skill_usage: None,
        mention_candidates: &[],
    };
    app.thread_ui.composer.handle_paste(text, &ctx);
    true
}

fn handle_composer_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code == KeyCode::Esc {
        app.thread_ui.focus = ThreadFocus::Transcript;
        return true;
    }
    if is_clipboard_paste_key(key) {
        return handle_clipboard_paste(app);
    }
    let ctx = crate::widgets::composer::ComposerContext {
        skills: &[],
        skill_usage: None,
        mention_candidates: &[],
    };
    let event = app.thread_ui.composer.handle_key(key, &ctx);
    if let crate::widgets::composer::ComposerEvent::Submit { .. } = event {
        let text = app.thread_ui.composer.submission_text();
        let images = app.thread_ui.composer.image_inputs();
        let delivered = if !text.is_empty() || !images.is_empty() {
            submit_composer(app, text, images)
        } else {
            false
        };
        if delivered {
            app.thread_ui.composer.clear_content();
        }
    }
    true
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
            app.thread_ui.composer.attach_clipboard_image(png);
            true
        }
        Ok(crate::clipboard::ClipboardContent::Text(text)) if text.is_empty() => true,
        Ok(crate::clipboard::ClipboardContent::Text(text)) => handle_paste(app, &text),
        Err(error) => {
            app.notice = Some(format!("Could not paste: {error}"));
            true
        }
    }
}

fn submit_composer(app: &mut App, text: String, images: Vec<ImageInput>) -> bool {
    if app.thread_ui.pending_prompt.is_some() {
        app.notice = Some("a message is already being delivered".to_owned());
        return false;
    }
    let Some(run) = app.thread_ui.data.run.clone() else {
        return false;
    };
    let project = app.thread_ui.data.project.clone();
    let id = app.thread_ui.data.run_id.clone();
    let images = (!images.is_empty()).then_some(images);
    let pending_label = if text.is_empty() {
        "[Image]".to_owned()
    } else {
        text.clone()
    };
    if is_run_active(run.record.status) {
        app.thread_ui.set_pending_composer(pending_label);
        app.pending.push(PendingAction::SendMessage {
            project,
            id,
            input: MessageInput {
                text: Some(text),
                images,
            },
        });
        true
    } else if last_session_id(&run.record).is_some() {
        app.thread_ui.set_pending_composer(pending_label);
        app.pending.push(PendingAction::ContinueRun {
            project,
            id,
            text: Some(text),
            images,
        });
        true
    } else {
        app.notice =
            Some("this task has no live session; start a new task for this follow-up".to_owned());
        false
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
            images: None,
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
        ThreadAction::ToggleTimelineItem(index) => {
            app.thread_ui.transcript.select(index);
            app.thread_ui.transcript.toggle_selected();
        }
        ThreadAction::OpenSubagent(id) => app.thread_ui.subagent_sheet = Some(id),
        ThreadAction::CloseSubagentSheet => app.thread_ui.subagent_sheet = None,
        ThreadAction::ToggleStepRail => {
            app.thread_ui.steps_collapsed = !app.thread_ui.steps_collapsed
        }
        ThreadAction::FocusComposer => {
            app.thread_ui.focus = ThreadFocus::Composer;
            app.thread_ui.composer.focus();
        }
        ThreadAction::FocusReviewNotes => app.thread_ui.focus = ThreadFocus::ReviewNotes,
        ThreadAction::ReviewSendBack => apply_action(app, ThreadAction::ReviewSendBack),
        ThreadAction::OpenGitTab(tab) => {
            let project = app.thread_ui.data.project.clone();
            let id = app.thread_ui.data.run_id.clone();
            crate::screens::task_git::open(app, &project, &id, tab);
        }
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
            images: None,
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
                images: None,
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
        | ThreadAction::ToggleTimelineItem(_)
        | ThreadAction::OpenSubagent(_)
        | ThreadAction::CloseSubagentSheet
        | ThreadAction::ToggleStepRail
        | ThreadAction::FocusComposer
        | ThreadAction::FocusReviewNotes
        | ThreadAction::OpenGitTab(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use coducktor_contract::{RunRecord, StepKind, StepState, StepStatus};
    use crossterm::event::{Event, MouseEvent, MouseEventKind};
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
                    route_key: None,
                    recovery_generation: None,
                    routing_intent: None,
                    routing_decision: None,
                    routing_wait: None,
                    routing_attempts: None,
                    extra: serde_json::Map::new(),
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
            None,
        );
        app.navigate_route(crate::app::Route::Thread {
            project: "main".to_owned(),
            id: "run-1".to_owned(),
        });
        app
    }

    #[test]
    fn mouse_wheel_scrolls_the_transcript() {
        let mut app = app_with_run(RunStatus::Running);
        for index in 0..50 {
            app.thread_ui
                .transcript
                .push(TranscriptItem::Note(NoteItem::new(
                    format!("n{index}"),
                    format!("note {index}"),
                    TranscriptNoteTone::Dim,
                )));
        }
        app.thread_ui.transcript_area = Some(Rect::new(10, 5, 60, 10));
        app.thread_ui.transcript.scroll_by(10);

        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 20,
            row: 8,
            modifiers: KeyModifiers::NONE,
        }));

        assert!(!app.thread_ui.transcript.at_top());
    }

    #[test]
    fn opening_a_cached_task_paints_its_prompt_before_history_loads() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        let cached = run(RunStatus::Running, "Fix the task experience");
        app.project_tasks
            .entry("main".to_owned())
            .or_default()
            .runs
            .push(cached);

        open(&mut app, "main", "run-1");

        assert_eq!(
            app.thread_ui
                .data
                .run
                .as_ref()
                .map(|run| run.record.task.as_str()),
            Some("Fix the task experience")
        );
        assert!(matches!(
            app.pending.as_slice(),
            [PendingAction::LoadThread { project, id }]
                if project == "main" && id == "run-1"
        ));
        let content = render_to_string(&mut app);
        assert!(content.contains("Fix the task experience"));
        assert!(!content.contains("Loading"));
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
        app.navigate_route(crate::app::Route::Thread {
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
                images: None,
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
    fn renders_at_the_three_snapshot_sizes_running_with_plan_and_steps() {
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
    fn renders_at_the_three_snapshot_sizes_review() {
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

    #[test]
    fn earlier_history_merges_by_sequence_without_duplicates_and_retains_cursor_state() {
        let mut app = app_with_run(RunStatus::Done);
        app.thread_ui.data.events = (101..=150)
            .map(|seq| {
                event(
                    f64::from(seq),
                    "note",
                    json!({"message": format!("event {seq}")}),
                )
            })
            .collect();
        app.thread_ui.data.older_cursor = Some("page-2".to_owned());
        assert_eq!(
            app.thread_ui.begin_load_earlier().as_deref(),
            Some("page-2")
        );
        app.thread_ui.merge_earlier(
            (1..=101)
                .map(|seq| {
                    event(
                        f64::from(seq),
                        "note",
                        json!({"message": format!("event {seq}")}),
                    )
                })
                .collect(),
            Some("page-3".to_owned()),
        );
        assert_eq!(app.thread_ui.data.events.len(), 150);
        assert!(
            app.thread_ui
                .data
                .events
                .windows(2)
                .all(|pair| pair[0].seq < pair[1].seq)
        );
        assert_eq!(app.thread_ui.data.older_cursor.as_deref(), Some("page-3"));
        assert!(!app.thread_ui.data.older_loading);
    }

    #[test]
    fn send_failure_restores_the_draft_and_exposes_retry_mode() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.set_pending_prompt("keep going".to_owned());
        app.thread_ui.restore_pending_prompt("main", "run-1");
        assert_eq!(app.thread_ui.composer.text, "keep going");
        assert!(app.thread_ui.delivery_error);
        assert!(app.thread_ui.pending_prompt.is_none());
    }

    #[test]
    fn durable_follow_up_replaces_the_optimistic_copy_during_generation() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.push_event(
            1.0,
            event(1.0, "user-message", json!({"text": "follow up"})),
        );
        app.thread_ui.set_pending_prompt("follow up".to_owned());
        let optimistic = render_to_string(&mut app);
        assert_eq!(optimistic.matches("follow up").count(), 2);
        assert!(optimistic.contains("Sending…"));

        app.thread_ui.push_event(
            2.0,
            event(2.0, "user-message", json!({"text": "follow up"})),
        );

        let content = render_to_string(&mut app);
        assert_eq!(content.matches("follow up").count(), 2);
        assert!(!content.contains("Sending…"));
        assert!(app.thread_ui.pending_prompt.is_none());
    }

    #[test]
    fn durable_image_follow_up_replaces_the_optimistic_copy() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.set_pending_prompt("[Image]".to_owned());

        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "user-message",
                json!({"text": "", "imageCount": 1, "images": ["data:image/png;base64,AA=="]}),
            ),
        );

        let content = render_to_string(&mut app);
        assert!(!content.contains("Sending…"));
        assert!(app.thread_ui.pending_prompt.is_none());
    }

    #[test]
    fn waiting_session_does_not_leave_a_working_outcome_row() {
        let mut app = app_with_run(RunStatus::Waiting);
        app.thread_ui.push_event(
            1.0,
            event(1.0, "text", json!({"text": "What should I do next?"})),
        );

        let content = render_to_string(&mut app);
        assert!(content.contains("Waiting for your reply."));
        assert!(!content.contains("Working…"));
    }

    #[test]
    fn bracketed_paste_inserts_into_the_focused_composer() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.focus = ThreadFocus::Composer;
        app.thread_ui.composer.focus();
        app.handle_event(crossterm::event::Event::Paste("first\nsecond".to_owned()));

        assert_eq!(app.thread_ui.composer.text, "first\nsecond");
    }

    #[test]
    fn large_text_and_clipboard_image_are_delivered_and_restored_on_failure() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.focus = ThreadFocus::Composer;
        app.thread_ui.composer.focus();
        let pasted = "x".repeat(crate::widgets::composer::LARGE_PASTE_CHAR_THRESHOLD + 1);
        handle_paste(&mut app, &pasted);
        app.thread_ui.composer.attach_clipboard_image(vec![1, 2, 3]);

        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(PendingAction::SendMessage { input, .. }) = app.pending.first() else {
            panic!("a message is queued");
        };
        assert_eq!(input.text.as_deref(), Some(pasted.as_str()));
        assert_eq!(input.images.as_ref().map(Vec::len), Some(1));
        assert!(app.thread_ui.composer.text.is_empty());

        app.thread_ui.restore_pending_prompt("main", "run-1");
        assert!(app.thread_ui.composer.text.contains("[Pasted Content "));
        assert!(app.thread_ui.composer.text.contains("[Image #1]"));
        assert_eq!(app.thread_ui.composer.submission_text(), pasted);
        assert_eq!(app.thread_ui.composer.image_inputs().len(), 1);
    }
}
