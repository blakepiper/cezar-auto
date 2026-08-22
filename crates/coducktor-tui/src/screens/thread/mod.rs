//! The task thread screen: the full run lifecycle, including actions, steps, plans, subagents,
//! questions, review, queued messages, auto-resume, and composition. Pure policies live in
//! `actions.rs` and `reducer.rs`; rendering helpers live in `widgets.rs`.
//!
//! The review panel exposes its actions without embedding a full diff. The composer sends text,
//! and the task Git tabs provide Changes, Files, and Commits navigation.

use std::path::PathBuf;
use std::time::{Duration, Instant};

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

use actions::is_run_active;
use projection::ThreadViewModel;
use reducer::{
    NoteTone, ThreadAsk, ThreadEntry, ThreadReduceOptions, ThreadState, reduce_thread,
    reduce_thread_incremental, strip_done_marker,
};

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

/// Sanitized local accounting for thread projection work. It intentionally records only counts
/// and elapsed time, never prompts, provider payloads, or transcript contents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadProjectionMetrics {
    pub rebuilds: usize,
    /// Number of durable events folded across all rebuilds. This makes accidental quadratic
    /// re-folding visible without retaining any event data.
    pub rebuilt_events: usize,
    /// Time spent folding durable events into `ThreadState`.
    pub rebuild_time: Duration,
    /// End-to-end projection and transcript reconciliation time.
    pub projection_time: Duration,
}

/// Result of folding one live batch. A sequence hole leaves the watermark at the last durable
/// contiguous event so the runtime can reload from that point instead of cementing the gap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadPushResult {
    pub accepted: usize,
    pub dropped_events: usize,
    pub refresh_required: bool,
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
    pending_prompt_queued: bool,
    pending_composer: Option<crate::widgets::composer::Composer>,
    pub delivery_error: bool,
    pub cancel_pending: bool,
    pub project_root: Option<PathBuf>,
    projection_metrics: ThreadProjectionMetrics,
    load_revision: u64,
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
            pending_prompt_queued: false,
            pending_composer: None,
            delivery_error: false,
            cancel_pending: false,
            project_root: None,
            projection_metrics: ThreadProjectionMetrics::default(),
            load_revision: 0,
        }
    }
}

impl ThreadUi {
    /// Return local projection accounting for diagnostics and scaling tests.
    pub fn projection_metrics(&self) -> ThreadProjectionMetrics {
        self.projection_metrics
    }

    pub fn load_revision(&self) -> u64 {
        self.load_revision
    }

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
        self.focus = if same_thread {
            self.focus
        } else {
            ThreadFocus::Composer
        };
        if self.focus == ThreadFocus::Composer {
            self.composer.focus();
        } else {
            self.composer.blur();
        }
        self.transcript_area = None;
        if !same_thread {
            self.pending_prompt = None;
            self.pending_prompt_after_seq = -1.0;
            self.pending_prompt_queued = false;
            self.pending_composer = None;
        }
        self.cancel_pending = false;
        self.load_revision = self.load_revision.wrapping_add(1);
        self.rebuild_full();
    }

    pub fn set_pending_prompt(&mut self, text: String) {
        self.pending_prompt = Some(text);
        self.pending_prompt_after_seq = self.data.as_of_seq;
        self.pending_prompt_queued = false;
        self.pending_composer = None;
        self.delivery_error = false;
        self.refresh_projection(0, Duration::ZERO);
    }

    fn set_pending_composer(&mut self, text: String, queued: bool) {
        self.pending_prompt = Some(text);
        self.pending_prompt_after_seq = self.data.as_of_seq;
        self.pending_prompt_queued = queued;
        self.pending_composer = Some(self.composer.clone());
        self.delivery_error = false;
        self.refresh_projection(0, Duration::ZERO);
    }

    pub fn set_project_root(&mut self, root: Option<PathBuf>) {
        self.project_root = root;
        self.refresh_projection(0, Duration::ZERO);
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
        self.pending_prompt_queued = false;
        self.delivery_error = true;
        self.refresh_projection(0, Duration::ZERO);
    }

    /// Replace the loaded run record (a fresh engine read or a workspace run event
    /// for the currently-open thread).
    pub fn set_run(&mut self, run: ApiRun) {
        self.data.run = Some(run);
        self.refresh_projection(0, Duration::ZERO);
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
        self.pending_prompt_queued = false;
        self.pending_composer = None;
        self.delivery_error = false;
    }

    /// Append one live run event and re-fold.
    pub fn push_event(&mut self, seq: f64, event: RunEvent) -> ThreadPushResult {
        self.push_events(std::iter::once((seq, event)))
    }

    /// Append a frame-sized live batch and re-fold once. This keeps projection cost linear in
    /// frames rather than rebuilding the complete transcript for every provider delta.
    pub fn push_events(
        &mut self,
        events: impl IntoIterator<Item = (f64, RunEvent)>,
    ) -> ThreadPushResult {
        let first_new = self.data.events.len();
        let mut result = ThreadPushResult::default();
        for (seq, event) in events {
            if seq <= self.data.as_of_seq {
                continue;
            }
            let expected = self.data.as_of_seq + 1.0;
            if self.data.as_of_seq >= 0.0 && seq > expected {
                result.dropped_events = (seq - expected).round().max(1.0) as usize;
                result.refresh_required = true;
                break;
            }
            self.data.as_of_seq = seq;
            self.data.events.push(event);
        }
        let accepted_len = self.data.events.len().saturating_sub(first_new);
        result.accepted = accepted_len;
        if accepted_len > 0 {
            let active_turn = self
                .data
                .run
                .as_ref()
                .is_some_and(|run| run.record.status == RunStatus::Running);
            let started = Instant::now();
            reduce_thread_incremental(
                &mut self.data.state,
                &self.data.events[first_new..],
                ThreadReduceOptions { active_turn },
            );
            self.refresh_projection(accepted_len, started.elapsed());
        }
        result
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
        self.rebuild_full();
        self.transcript.preserve_after_prepend(added);
    }

    pub fn fail_load_earlier(&mut self, error: String) {
        self.data.older_loading = false;
        self.data.older_error = Some(error);
    }

    fn rebuild_full(&mut self) {
        let active_turn = self
            .data
            .run
            .as_ref()
            .is_some_and(|run| run.record.status == RunStatus::Running);
        let started = Instant::now();
        self.data.state = reduce_thread(&self.data.events, ThreadReduceOptions { active_turn });
        self.refresh_projection(self.data.events.len(), started.elapsed());
    }

    fn refresh_projection(&mut self, reduced_events: usize, reduction_time: Duration) {
        let started = Instant::now();
        // The send request being accepted does not mean its user-message has reached the
        // transcript yet. Keep the optimistic prompt visible until the durable event arrives,
        // then let that event replace it without briefly hiding what the agent is working on.
        let queued_prompt_is_durable = self.pending_prompt_queued
            && self.pending_prompt.as_deref().is_some_and(|prompt| {
                self.data.run.as_ref().is_some_and(|run| {
                    run.record
                        .queued_messages
                        .iter()
                        .flatten()
                        .any(|message| queued_prompt_label(message) == prompt)
                })
            });
        let pending_is_durable = queued_prompt_is_durable
            || self.pending_prompt.as_deref().is_some_and(|prompt| {
                self.data.events.iter().any(|event| {
                event.seq > self.pending_prompt_after_seq
                    && event.event_type == "user-message"
                    && durable_prompt_label(event) == prompt
            })
            // History is paged. A reload can therefore advance its sequence watermark past the
            // follow-up while omitting that individual event from the visible page. The core
            // writes the user-message before it can append any later event, so this is still a
            // durable acknowledgement; without it the optimistic UI could remain "Sending…"
            // forever after a missed live notification.
            || (!self.pending_prompt_queued
                && self.data.as_of_seq > self.pending_prompt_after_seq)
            });
        if pending_is_durable {
            self.pending_prompt = None;
            self.pending_prompt_after_seq = -1.0;
            self.pending_prompt_queued = false;
            self.pending_composer = None;
            self.delivery_error = false;
        }
        if let Some(run) = &self.data.run {
            self.data.view_model = projection::project_thread_with_root(
                run,
                &self.data.state,
                self.project_root.as_deref(),
            );
            self.transcript.reconcile_reusing(|existing| {
                build_transcript_items(
                    run,
                    &self.data.state,
                    &self.data.view_model,
                    EarlierHistory {
                        available: self.data.older_cursor.is_some(),
                        loading: self.data.older_loading,
                        error: self.data.older_error.as_deref(),
                    },
                    self.pending_prompt.as_deref(),
                    self.pending_prompt_queued,
                    existing,
                )
            });
            // A resolved ask, or a fresh one under a different id, clears any stale in-progress
            // selections — a leftover partial answer must never attach to the NEXT question.
            if pending_ask(&self.data.state).is_none() {
                self.ask_selections.clear();
            }
        }
        self.projection_metrics.rebuilds += 1;
        self.projection_metrics.rebuilt_events += reduced_events;
        self.projection_metrics.rebuild_time += reduction_time;
        self.projection_metrics.projection_time += started.elapsed();
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

fn queued_prompt_label(message: &coducktor_contract::QueuedMessage) -> &str {
    if message.text.is_empty()
        && message
            .images
            .as_ref()
            .is_some_and(|images| !images.is_empty())
    {
        "[Image]"
    } else {
        &message.text
    }
}

fn map_tone(tone: NoteTone) -> TranscriptNoteTone {
    match tone {
        NoteTone::Dim => TranscriptNoteTone::Dim,
        NoteTone::Warning => TranscriptNoteTone::Warning,
        NoteTone::Danger => TranscriptNoteTone::Danger,
    }
}

struct EarlierHistory<'a> {
    available: bool,
    loading: bool,
    error: Option<&'a str>,
}

fn build_transcript_items(
    run: &ApiRun,
    state: &ThreadState,
    view_model: &ThreadViewModel,
    earlier: EarlierHistory<'_>,
    pending_prompt: Option<&str>,
    pending_prompt_queued: bool,
    existing: &mut std::collections::HashMap<String, TranscriptItem>,
) -> Vec<TranscriptItem> {
    let mut items = Vec::new();
    if earlier.loading {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-loading",
            "Loading earlier history…",
            TranscriptNoteTone::Dim,
        )));
    } else if let Some(error) = earlier.error {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-error",
            format!("Earlier history failed: {error} — press R to retry"),
            TranscriptNoteTone::Danger,
        )));
    } else if earlier.available {
        items.push(TranscriptItem::Note(NoteItem::new(
            "history-earlier",
            "Earlier history available — scroll to the top or press R to load",
            TranscriptNoteTone::Dim,
        )));
    }
    if !run.record.task.trim().is_empty() {
        items.push(reuse_message(
            existing,
            "task".to_owned(),
            coducktor_protocol::MessageRole::User,
            &run.record.task,
        ));
    }
    for (turn_index, turn) in state.turns.iter().enumerate() {
        let has_ask_card = turn
            .items
            .iter()
            .any(|item| matches!(item, ThreadEntry::Ask(_)));
        let active_last_turn =
            run.record.status == RunStatus::Running && turn_index + 1 == state.turns.len();
        let strip_ask = has_ask_card || active_last_turn;
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
            items.push(reuse_message(
                existing,
                format!("um:{}", turn.id),
                coducktor_protocol::MessageRole::User,
                &user_message.text,
            ));
        }
        for entry in &turn.items {
            match entry {
                ThreadEntry::Item(UiItem::Message(message)) => {
                    let text = if message.role == coducktor_protocol::MessageRole::Assistant
                        && message.text.contains("DUCK:")
                    {
                        std::borrow::Cow::Owned(strip_done_marker(&message.text, strip_ask))
                    } else {
                        std::borrow::Cow::Borrowed(message.text.as_str())
                    };
                    items.push(reuse_message(
                        existing,
                        message.id.clone(),
                        message.role,
                        &text,
                    ));
                }
                ThreadEntry::Item(UiItem::Reasoning(reasoning)) => {
                    items.push(reuse_reasoning(
                        existing,
                        reasoning.id.clone(),
                        &reasoning.text,
                    ));
                }
                ThreadEntry::Item(UiItem::Tool(tool)) => {
                    items.push(reuse_tool(existing, tool));
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
    for message in run.record.queued_messages.iter().flatten() {
        items.push(reuse_message(
            existing,
            format!("queued:{}", message.id),
            coducktor_protocol::MessageRole::User,
            queued_prompt_label(message),
        ));
        items.push(TranscriptItem::Note(NoteItem::new(
            format!("queued-state:{}", message.id),
            "Queued for the next turn",
            TranscriptNoteTone::Dim,
        )));
    }
    if let Some(prompt) = pending_prompt {
        items.push(TranscriptItem::Message(MessageItem::new(
            "pending-prompt",
            coducktor_protocol::MessageRole::User,
            prompt,
        )));
        items.push(TranscriptItem::Note(NoteItem::new(
            "pending-prompt-state",
            if pending_prompt_queued {
                "Queueing…"
            } else {
                "Sending…"
            },
            TranscriptNoteTone::Dim,
        )));
    }
    let mut latest_tool = None;
    for (index, item) in items.iter_mut().enumerate() {
        if let TranscriptItem::Tool(tool) = item {
            tool.is_latest = false;
            latest_tool = Some(index);
        }
    }
    if let Some(index) = latest_tool
        && let Some(TranscriptItem::Tool(item)) = items.get_mut(index)
    {
        item.is_latest = true;
    }
    items
}

fn reuse_message(
    existing: &mut std::collections::HashMap<String, TranscriptItem>,
    id: String,
    role: coducktor_protocol::MessageRole,
    text: &str,
) -> TranscriptItem {
    if let Some(TranscriptItem::Message(item)) = existing.remove(&id)
        && item.role == role
        && item.text == text
    {
        return TranscriptItem::Message(item);
    }
    TranscriptItem::Message(MessageItem::new(id, role, text))
}

fn reuse_reasoning(
    existing: &mut std::collections::HashMap<String, TranscriptItem>,
    id: String,
    text: &str,
) -> TranscriptItem {
    if let Some(TranscriptItem::Reasoning(item)) = existing.remove(&id)
        && item.text == text
    {
        return TranscriptItem::Reasoning(item);
    }
    TranscriptItem::Reasoning(ReasoningItem::new(id, text))
}

fn reuse_tool(
    existing: &mut std::collections::HashMap<String, TranscriptItem>,
    tool: &coducktor_protocol::UiToolItem,
) -> TranscriptItem {
    let id = tool.id.clone();
    let candidate = ToolItem::new(id.clone(), &tool.name, tool.input.as_ref(), tool.status);
    if let Some(TranscriptItem::Tool(item)) = existing.remove(&id)
        && item.tool_kind == candidate.tool_kind
        && item.title == candidate.title
        && item.subtitle == candidate.subtitle
        && item.status == tool.status
        && item.output.as_deref() == tool.output.as_deref()
        && item.error.as_deref() == tool.error.as_deref()
        && item.exit_code == tool.exit_code.map(|value| value as i64)
    {
        return TranscriptItem::Tool(item);
    }
    let mut candidate = candidate;
    candidate.output = tool.output.clone();
    candidate.error = tool.error.clone();
    candidate.exit_code = tool.exit_code.map(|value| value as i64);
    TranscriptItem::Tool(candidate)
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
    let live_activity_height = u16::from(matches!(
        record.status,
        RunStatus::Queued | RunStatus::Running
    ));
    let composer_height = app
        .thread_ui
        .composer
        .height_for_width(area.width.saturating_sub(3))
        + 2;
    let base_dock_height = ask_height
        + review_height
        + auto_resume_height
        + hint_height
        + live_activity_height
        + composer_height;

    let base_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(step_rail_height),
            Constraint::Min(3),
            Constraint::Length(base_dock_height),
        ])
        .split(area);
    let latest_message = if record.status == RunStatus::Waiting {
        app.thread_ui
            .transcript
            .latest_assistant_message()
            .map(|(id, text)| (id.to_owned(), text.to_owned()))
            .filter(|(id, _)| {
                !app.thread_ui.transcript.item_content_fully_visible(
                    id,
                    area.width,
                    base_rows[2].height.saturating_sub(1),
                )
            })
    } else {
        None
    };
    let latest_message_height = latest_message
        .as_ref()
        .map(|(_, text)| widgets::latest_message_height(text, area.width))
        .unwrap_or(0);
    let dock_height = base_dock_height + latest_message_height;

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
        let mut detail = Vec::new();
        if let Some(started_at) = record.started_at.as_deref() {
            let elapsed = crate::screens::runs_util::short_age(started_at, app.now_epoch);
            if !elapsed.is_empty() {
                detail.push(elapsed);
            }
        }
        if detail.is_empty() {
            detail.push("0s".to_owned());
        }
        detail.push(format!("{} tok", record.tokens_used as i64));
        format!(
            "{throbber} {} · {}",
            app.thread_ui.data.view_model.current_status,
            detail.join(" · ")
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
            Constraint::Length(latest_message_height),
            Constraint::Length(hint_height),
            Constraint::Length(live_activity_height),
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
    if let Some((_, message)) = &latest_message {
        widgets::render_latest_message(frame, dock_rows[3], message, &theme);
    }
    if hint_height > 0 {
        let text = match record.status {
            RunStatus::Queued if app.thread_ui.cancel_pending => "Stopping the agent…",
            RunStatus::Queued => widgets::queue_hint(true),
            RunStatus::Idle => "Session is ready for a follow-up.",
            RunStatus::Waiting => "Waiting for your reply.",
            RunStatus::Running if app.thread_ui.cancel_pending => "Stopping the agent…",
            RunStatus::Running => "Agent is working · Enter queues a follow-up for the next turn.",
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => {
                "Enter starts a follow-up turn."
            }
            RunStatus::Review => "Enter starts a follow-up; review actions are above.",
        };
        widgets::render_status_hint(frame, dock_rows[4], text, &theme);
    }
    if live_activity_height > 0 {
        let mut detail = Vec::new();
        if let Some(started_at) = record.started_at.as_deref() {
            let elapsed = crate::screens::runs_util::short_age(started_at, app.now_epoch);
            if !elapsed.is_empty() {
                detail.push(elapsed);
            }
        }
        if detail.is_empty() {
            detail.push("0s".to_owned());
        }
        detail.push(format!("{} tok", record.tokens_used as i64));
        if let Some(tool) = app.thread_ui.transcript.latest_running_tool_title() {
            detail.push(tool.to_owned());
        }
        let phase = if app.thread_ui.cancel_pending {
            "Stopping…"
        } else {
            app.thread_ui.data.view_model.current_status.as_str()
        };
        let activity = if detail.is_empty() {
            phase.to_owned()
        } else {
            format!("{phase} · {}", detail.join(" · "))
        };
        widgets::render_live_activity(frame, dock_rows[5], &activity, &theme);
    }
    app.thread_ui
        .composer
        .set_title(if app.thread_ui.pending_prompt.is_some() {
            if app.thread_ui.pending_prompt_queued {
                "QUEUEING"
            } else {
                "SENDING"
            }
        } else if app.thread_ui.delivery_error {
            "SEND FAILED · RETRY"
        } else {
            match record.status {
                RunStatus::Queued => "FOLLOW UP",
                RunStatus::Idle => "FOLLOW UP",
                RunStatus::Waiting => "ANSWER",
                RunStatus::Running => "FOLLOW UP",
                RunStatus::Review | RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled => {
                    "FOLLOW UP"
                }
            }
        });
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
    if key.code == KeyCode::Esc
        && matches!(
            app.thread_ui.focus,
            ThreadFocus::Composer | ThreadFocus::Transcript
        )
    {
        return interrupt_or_focus_transcript(app);
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
        _ => false,
    }
}

fn interrupt_or_focus_transcript(app: &mut App) -> bool {
    let active = app
        .thread_ui
        .data
        .run
        .as_ref()
        .is_some_and(|run| matches!(run.record.status, RunStatus::Queued | RunStatus::Running));
    if active && !app.thread_ui.cancel_pending {
        app.thread_ui.cancel_pending = true;
        app.pending.push(PendingAction::CancelRun {
            project: app.thread_ui.data.project.clone(),
            id: app.thread_ui.data.run_id.clone(),
        });
    } else {
        app.thread_ui.composer.blur();
        app.thread_ui.focus = ThreadFocus::Transcript;
    }
    true
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
        let queued = matches!(run.record.status, RunStatus::Queued | RunStatus::Running);
        app.thread_ui.set_pending_composer(pending_label, queued);
        app.pending.push(PendingAction::SendMessage {
            project,
            id,
            input: MessageInput {
                text: Some(text),
                images,
            },
        });
        true
    } else {
        // No prior session just means the engine starts a fresh step in this same run/worktree
        // instead of resuming a transcript — still delivered as a follow-up, not a new task.
        app.thread_ui.set_pending_composer(pending_label, false);
        app.pending.push(PendingAction::ContinueRun {
            project,
            id,
            text: Some(text),
            images,
        });
        true
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
                    routing_decision: None,
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

    fn realistic_v2_event(sequence: u64) -> RunEvent {
        let item_index = sequence / 32;
        let offset = sequence % 32;
        let is_tool = item_index.is_multiple_of(2);
        let id = if is_tool {
            format!("tool-{item_index}")
        } else {
            format!("message-{item_index}")
        };
        let (event_type, extra) = match offset {
            0 if is_tool => (
                "item.started",
                json!({"item": {"kind": "tool", "id": id, "name": "Bash", "toolKind": "execute", "title": "Run cargo test --workspace", "status": "running", "input": {"command": "cargo test --workspace"}}}),
            ),
            0 => (
                "item.started",
                json!({"item": {"kind": "message", "id": id, "role": "assistant", "text": "", "phase": "commentary"}}),
            ),
            31 if is_tool => (
                "item.completed",
                json!({"item": {"kind": "tool", "id": id, "name": "Bash", "toolKind": "execute", "title": "Run cargo test --workspace", "status": "completed", "input": {"command": "cargo test --workspace"}, "output": (0..80).map(|line| format!("test case {line}: ok")).collect::<Vec<_>>().join("\n"), "exitCode": 0}}),
            ),
            31 => (
                "item.completed",
                json!({"item": {"kind": "message", "id": id, "role": "assistant", "text": "### Progress\n\nThe focused change is in place, and the relevant tests are passing.", "phase": "commentary"}}),
            ),
            _ if is_tool => (
                "item.delta",
                json!({"itemId": id, "field": "output", "delta": "checking target... ok\n"}),
            ),
            _ => (
                "item.delta",
                json!({"itemId": id, "field": "text", "delta": "Streaming markdown. "}),
            ),
        };
        event((sequence + 1) as f64, event_type, extra)
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
    fn a_large_live_batch_rebuilds_the_transcript_once() {
        let mut app = app_with_run(RunStatus::Running);
        let metrics_before = app.thread_ui.projection_metrics();

        app.thread_ui.push_events((1..=1_000).map(|seq| {
            let seq = f64::from(seq);
            (
                seq,
                event(seq, "text", json!({"text": format!("chunk {seq}")})),
            )
        }));

        let metrics = app.thread_ui.projection_metrics();
        assert_eq!(metrics.rebuilds - metrics_before.rebuilds, 1);
        assert_eq!(
            metrics.rebuilt_events - metrics_before.rebuilt_events,
            1_000
        );
        assert!(metrics.rebuild_time >= metrics_before.rebuild_time);
        assert_eq!(app.thread_ui.data.events.len(), 1_000);
        assert_eq!(app.thread_ui.data.as_of_seq, 1_000.0);
    }

    #[test]
    fn a_sequence_hole_preserves_the_last_good_watermark_until_durable_reload() {
        let durable = (1..=4)
            .map(|seq| {
                event(
                    seq as f64,
                    "note",
                    json!({"message": format!("line {seq}")}),
                )
            })
            .collect::<Vec<_>>();
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        app.thread_ui.load(
            "main".to_owned(),
            "run-1".to_owned(),
            run(RunStatus::Running, "Ship the shell"),
            durable[..2].to_vec(),
            2.0,
            None,
        );

        let result = app.thread_ui.push_event(4.0, durable[3].clone());
        assert_eq!(result.dropped_events, 1);
        assert!(result.refresh_required);
        assert_eq!(app.thread_ui.data.as_of_seq, 2.0);
        assert_eq!(app.thread_ui.data.events.len(), 2);

        app.thread_ui.load(
            "main".to_owned(),
            "run-1".to_owned(),
            run(RunStatus::Running, "Ship the shell"),
            durable,
            4.0,
            None,
        );
        assert_eq!(app.thread_ui.data.as_of_seq, 4.0);
        assert_eq!(app.thread_ui.data.events.len(), 4);
    }

    #[test]
    fn batched_projection_work_scales_linearly_with_accepted_events() {
        fn projection_work(event_count: u64) -> ThreadProjectionMetrics {
            let mut app = app_with_run(RunStatus::Running);
            let before = app.thread_ui.projection_metrics();
            app.thread_ui.push_events((1..=event_count).map(|seq| {
                let sequence = seq as f64;
                (sequence, event(sequence, "text", json!({"text": "delta"})))
            }));
            let after = app.thread_ui.projection_metrics();
            ThreadProjectionMetrics {
                rebuilds: after.rebuilds - before.rebuilds,
                rebuilt_events: after.rebuilt_events - before.rebuilt_events,
                rebuild_time: after.rebuild_time.saturating_sub(before.rebuild_time),
                projection_time: after.projection_time.saturating_sub(before.projection_time),
            }
        }

        let small = projection_work(1_000);
        let large = projection_work(2_000);
        assert_eq!(small.rebuilds, 1);
        assert_eq!(large.rebuilds, 1);
        assert_eq!(small.rebuilt_events, 1_000);
        assert_eq!(large.rebuilt_events, 2_000);
    }

    /// Production-shape guard: events arrive across many frame-sized batches, rather than in one
    /// call that can only demonstrate a linear single fold.
    #[test]
    fn doubling_accepted_events_does_not_quadruple_rebuild_time() {
        fn projection_work(event_count: u64) -> ThreadProjectionMetrics {
            let mut app = app_with_run(RunStatus::Running);
            let before = app.thread_ui.projection_metrics();
            for first in (0..event_count).step_by(8) {
                let end = (first + 8).min(event_count);
                app.thread_ui.push_events((first..end).map(|sequence| {
                    let event = realistic_v2_event(sequence);
                    (event.seq, event)
                }));
            }
            let after = app.thread_ui.projection_metrics();
            ThreadProjectionMetrics {
                rebuilds: after.rebuilds - before.rebuilds,
                rebuilt_events: after.rebuilt_events - before.rebuilt_events,
                rebuild_time: after.rebuild_time.saturating_sub(before.rebuild_time),
                projection_time: after.projection_time.saturating_sub(before.projection_time),
            }
        }

        let small = projection_work(1_000);
        let large = projection_work(2_000);
        assert_eq!(small.rebuilds, 125);
        assert_eq!(large.rebuilds, 250);
        assert_eq!(small.rebuilt_events, 1_000);
        assert_eq!(large.rebuilt_events, 2_000);
        assert!(
            large.rebuilt_events <= small.rebuilt_events * 3
                && large.rebuild_time < small.rebuild_time * 3,
            "doubling accepted events rebuilt {large:?} against {small:?}; frame-batched \
             projection is still quadratic"
        );
    }

    #[test]
    fn live_thread_frame_at_twelve_thousand_events_stays_under_eight_ms() {
        use ratatui::buffer::Buffer;

        let mut app = app_with_run(RunStatus::Running);
        let theme = Theme::detect();
        let viewport = Rect::new(0, 0, 120, 40);
        for first in (0..11_992_u64).step_by(8) {
            app.thread_ui
                .push_events((first..first + 8).map(|sequence| {
                    let event = realistic_v2_event(sequence);
                    (event.seq, event)
                }));
            let mut buffer = Buffer::empty(viewport);
            app.thread_ui
                .transcript
                .render(&mut buffer, viewport, &theme);
        }

        let started = Instant::now();
        app.thread_ui.push_events((11_992..12_000).map(|sequence| {
            let event = realistic_v2_event(sequence);
            (event.seq, event)
        }));
        let mut buffer = Buffer::empty(viewport);
        app.thread_ui
            .transcript
            .render(&mut buffer, viewport, &theme);
        let elapsed = started.elapsed();
        let budget = if cfg!(debug_assertions) {
            Duration::from_millis(30)
        } else {
            Duration::from_millis(8)
        };
        assert!(
            elapsed < budget,
            "12,000-event live frame took {elapsed:?}; target is <8ms in the optimized profile"
        );
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
    fn a_session_scoped_permission_option_is_flagged_as_persistent() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "permission.requested",
                json!({
                    "requestId": "permission-1",
                    "title": "Allow command: cargo test?",
                    "options": [
                        {"id": "allow_once", "label": "Allow once", "kind": "allow_once"},
                        {"id": "allow_session", "label": "Allow session", "kind": "allow_always"},
                        {"id": "reject_once", "label": "Reject", "kind": "reject_once"}
                    ]
                }),
            ),
        );
        let content = render_to_string(&mut app);
        assert!(
            content.contains("Allow session (remembers this choice)"),
            "a session-scoped grant is flagged as persistent: {content}"
        );
        assert!(
            !content.contains("Allow once (remembers this choice)"),
            "a once-only grant is not: {content}"
        );
    }

    #[test]
    fn a_multi_line_routing_note_renders_every_considered_candidate() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "note",
                json!({"message": "Auto routing · selected Codex\n  Claude — reserved quota"}),
            ),
        );
        let content = render_to_string(&mut app);
        assert!(
            content.contains("Auto routing · selected Codex"),
            "{content}"
        );
        assert!(content.contains("Claude — reserved quota"), "{content}");
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
    fn a_fresh_thread_focuses_the_composer_and_refresh_preserves_focus() {
        let mut app = app_with_run(RunStatus::Running);
        assert_eq!(app.thread_ui.focus, ThreadFocus::Composer);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.thread_ui.composer.text, "x");

        app.thread_ui.focus = ThreadFocus::Transcript;
        app.thread_ui.composer.blur();
        let current = app.thread_ui.data.run.clone().unwrap();
        app.thread_ui.load(
            "main".to_owned(),
            "run-1".to_owned(),
            current,
            Vec::new(),
            -1.0,
            None,
        );
        assert_eq!(app.thread_ui.focus, ThreadFocus::Transcript);
    }

    #[test]
    fn escape_interrupts_once_then_moves_focus_to_the_transcript() {
        let mut app = app_with_run(RunStatus::Running);
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert!(app.thread_ui.cancel_pending);
        assert_eq!(app.thread_ui.focus, ThreadFocus::Composer);
        assert!(matches!(
            app.pending.as_slice(),
            [PendingAction::CancelRun { project, id }]
                if project == "main" && id == "run-1"
        ));
        assert!(app.confirm.is_none());

        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert_eq!(app.thread_ui.focus, ThreadFocus::Transcript);
        assert_eq!(app.pending.len(), 1);

        let mut queued = app_with_run(RunStatus::Queued);
        queued.thread_ui.focus = ThreadFocus::Transcript;
        queued.thread_ui.composer.blur();
        assert!(handle_key(
            &mut queued,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert!(queued.thread_ui.cancel_pending);
        assert!(matches!(
            queued.pending.as_slice(),
            [PendingAction::CancelRun { project, id }]
                if project == "main" && id == "run-1"
        ));
    }

    #[test]
    fn idle_escape_only_drops_focus_and_bare_archive_finish_keys_are_inert() {
        let mut app = app_with_run(RunStatus::Idle);
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert_eq!(app.thread_ui.focus, ThreadFocus::Transcript);
        assert!(app.pending.is_empty());

        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
        ));
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)
        ));
        assert!(app.pending.is_empty());
        assert!(app.confirm.is_none());
    }

    #[test]
    fn running_follow_up_is_optimistic_then_visible_in_the_durable_queue() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.composer.set_text("check the compact layout");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.pending.as_slice(),
            [PendingAction::SendMessage { input, .. }]
                if input.text.as_deref() == Some("check the compact layout")
        ));
        let pending = render_to_string(&mut app);
        assert!(pending.contains("Queueing…"));
        assert!(pending.contains("QUEUEING"));

        let mut durable = app.thread_ui.data.run.clone().unwrap();
        durable.record.queued_messages = Some(vec![coducktor_contract::QueuedMessage {
            id: "queued-1".to_owned(),
            text: "check the compact layout".to_owned(),
            images: None,
            created_at: "2026-08-15T00:00:01Z".to_owned(),
        }]);
        app.thread_ui.set_run(durable);
        let content = render_to_string(&mut app);
        assert!(app.thread_ui.pending_prompt.is_none());
        assert!(content.contains("Queued for the next turn"));
        assert_eq!(content.matches("check the compact layout").count(), 1);
    }

    #[test]
    fn live_activity_line_includes_the_running_tool_and_usage() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "item.started",
                json!({"item": {"kind": "tool", "id": "t1", "name": "Bash", "toolKind": "execute", "title": "Run cargo test -p coducktor-tui", "status": "running", "input": {"command": "cargo test -p coducktor-tui"}}}),
            ),
        );
        let content = render_to_string(&mut app);
        assert!(
            content.contains(
                "● Running cargo test -p coducktor-tui · 0s · 0 tok · Ran cargo test -p coducktor-tui"
            ),
            "rendered thread: {content}"
        );
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
    fn the_most_recently_appended_tool_call_is_marked_latest_and_expands() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "item.completed",
                json!({"item": {"kind": "tool", "id": "t1", "name": "Read", "toolKind": "read", "title": "Read a.rs", "status": "completed", "output": "fn a() {}"}}),
            ),
        );
        app.thread_ui.push_event(
            2.0,
            event(
                2.0,
                "item.completed",
                json!({"item": {"kind": "tool", "id": "t2", "name": "Read", "toolKind": "read", "title": "Read b.rs", "status": "completed", "output": "fn b() {}"}}),
            ),
        );

        let tools: Vec<_> = app
            .thread_ui
            .transcript
            .items()
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect();
        assert_eq!(tools.len(), 2);
        assert!(!tools[0].is_latest, "the earlier call is not latest");
        assert!(tools[1].is_latest, "the last-appended call is latest");

        let content = render_to_string(&mut app);
        assert!(
            content.contains("fn b() {}"),
            "the latest tool call's output is expanded by default"
        );
        assert!(
            !content.contains("fn a() {}"),
            "the earlier tool call stays collapsed"
        );
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
    fn history_watermark_acknowledges_a_follow_up_missing_from_the_visible_page() {
        let mut app = app_with_run(RunStatus::Running);
        app.thread_ui
            .push_event(1.0, event(1.0, "text", json!({"text": "earlier output"})));
        app.thread_ui.set_pending_prompt("follow up".to_owned());
        let run = app.thread_ui.data.run.clone().unwrap();

        // A compact history page can omit the just-written user event while its watermark still
        // includes it. That must acknowledge the optimistic composer state.
        app.thread_ui.load(
            "main".to_owned(),
            "run-1".to_owned(),
            run,
            vec![event(1.0, "text", json!({"text": "earlier output"}))],
            2.0,
            None,
        );

        assert!(app.thread_ui.pending_prompt.is_none());
        assert!(!app.thread_ui.delivery_error);
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
        assert!(
            !content.contains("LATEST MESSAGE"),
            "visible prose is not duplicated in the dock"
        );
    }

    #[test]
    fn waiting_session_keeps_agent_text_after_later_tool_activity() {
        let mut app = app_with_run(RunStatus::Waiting);
        app.thread_ui.push_event(
            1.0,
            event(
                1.0,
                "item.started",
                json!({
                    "item": {
                        "kind": "message",
                        "id": "agent-question",
                        "role": "assistant",
                        "text": "Which export format should I use?",
                        "phase": "commentary"
                    }
                }),
            ),
        );
        for index in 0..20 {
            let sequence = f64::from(index * 2 + 2);
            let id = format!("inspect-{index}");
            app.thread_ui.push_events([
                (
                    sequence,
                    event(
                        sequence,
                        "tool-call",
                        json!({"id": id.clone(), "tool": "shell", "input": {"cmd": "inspect"}}),
                    ),
                ),
                (
                    sequence + 1.0,
                    event(
                        sequence + 1.0,
                        "tool-result",
                        json!({"toolCallId": id, "result": "done"}),
                    ),
                ),
            ]);
        }

        let ids: Vec<_> = app
            .thread_ui
            .transcript
            .items()
            .iter()
            .map(TranscriptItem::id)
            .collect();
        assert!(
            ids.iter().position(|id| *id == "agent-question")
                < ids.iter().position(|id| *id == "inspect-0"),
            "the transcript preserves reducer chronology"
        );
        assert!(matches!(
            app.thread_ui
                .transcript
                .items()
                .iter()
                .find(|item| item.id() == "agent-question"),
            Some(TranscriptItem::Message(_))
        ));
        let content = render_to_string(&mut app);
        assert!(content.contains("LATEST MESSAGE"));
        assert!(content.contains("Which export format should I use?"));
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
