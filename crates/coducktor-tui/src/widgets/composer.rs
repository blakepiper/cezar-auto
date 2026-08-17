//! Shared composer widget for the New Task and thread screens. It owns terminal-native editing,
//! skill completion, quick replies, and image-file attachment behavior.
//!
//! The host owns the draft (`NewTaskDraft`): every `TextChanged` event tells it to
//! copy `composer.text` into the draft and persist it, so navigation never loses a
//! half-typed task.

use std::collections::BTreeMap;

use coducktor_contract::Skill;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::input::hitmap::{HitAction, HitMap};
use crate::skills::filter_skills;
use crate::theme::Theme;
use crate::widgets::picker::PickerItem;

/// An image attachment: a path the user named. The bytes are read and base64-encoded
/// at submit time by the host, so the widget itself stays pure and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub path: String,
}

/// The caret math behind `/` and `@` autocomplete: the token starts at the nearest trigger char
/// at a word boundary (start of text, or after whitespace/newline) scanning left from the caret,
/// and must contain no whitespace. Mid-word triggers stay inert.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerState {
    pub trigger: char,
    /// Byte index of the trigger character itself in the text.
    pub start: usize,
    /// What the user typed after the trigger, up to the caret — the filter query.
    pub query: String,
}

pub fn detect_trigger(text: &str, caret: usize) -> Option<TriggerState> {
    let chars: Vec<(usize, char)> = text
        .char_indices()
        .take_while(|(index, _)| *index < caret)
        .collect();
    for (index, character) in chars.iter().rev() {
        if character.is_whitespace() {
            return None;
        }
        if *character == '/' || *character == '@' {
            if *index > 0
                && let Some(previous) = text[..*index].chars().next_back()
                && !previous.is_whitespace()
            {
                return None; // mid-word: URL, e-mail, path…
            }
            return Some(TriggerState {
                trigger: *character,
                start: *index,
                query: text[index + character.len_utf8()..caret].to_owned(),
            });
        }
    }
    None
}

/// Replace the open token (trigger..caret) with the chosen completion plus one
/// trailing space, leaving everything after the caret untouched — the port of
/// `applyCompletion`.
pub fn apply_completion(
    text: &str,
    state: &TriggerState,
    caret: usize,
    completion: &str,
) -> (String, usize) {
    let inserted = format!("{}{} ", state.trigger, completion);
    let mut next = String::with_capacity(text.len() + inserted.len());
    next.push_str(&text[..state.start]);
    next.push_str(&inserted);
    next.push_str(&text[caret..]);
    (next, state.start + inserted.len())
}

/// What the composer tells the host after one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    /// The text changed (or attachments changed) — the host should sync the draft.
    Changed,
    /// The user asked to send the current draft.
    Submit { text: String },
    /// A canned quick reply (`Alt+A` / `Alt+C`).
    QuickReply { text: String },
    /// A `/`-menu pick counted a skill use (for the ui-state frequency sort).
    PickedSkill { name: String },
    /// Leave the composer (Esc / Tab).
    Blur,
}

/// The open `/`-or-`@` autocomplete menu.
#[derive(Debug, Clone)]
pub struct ComposerMenu {
    pub trigger: char,
    pub start: usize,
    pub query: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
}

/// Data a host must feed the composer so the `/` menu can rank candidates.
pub struct ComposerContext<'a> {
    pub skills: &'a [Skill],
    pub skill_usage: Option<&'a BTreeMap<String, f64>>,
    pub mention_candidates: &'a [String],
}

#[derive(Debug, Clone)]
pub struct Composer {
    pub text: String,
    /// Byte index of the caret.
    pub caret: usize,
    pub focused: bool,
    /// A path being typed in the attach prompt (`None` when no prompt is open).
    pub attaching: Option<String>,
    pub attachments: Vec<Attachment>,
    pub menu: Option<ComposerMenu>,
    /// `Alt+A` / `Alt+C` quick replies (the thread host wants them; the hero does not).
    pub quick_replies: bool,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            text: String::new(),
            caret: 0,
            focused: false,
            attaching: None,
            attachments: Vec::new(),
            menu: None,
            quick_replies: true,
        }
    }
}

impl Composer {
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
        self.caret = self.text.len();
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }

    /// Recompute the `/`-or-`@` autocomplete from the current text+caret. Called by
    /// the host after every text change and on focus.
    pub fn refresh_menu(&mut self, ctx: &ComposerContext<'_>) {
        let Some(state) = detect_trigger(&self.text, self.caret) else {
            self.menu = None;
            return;
        };
        let items = match state.trigger {
            '/' => filter_skills(ctx.skills, &state.query, ctx.skill_usage)
                .into_iter()
                .map(|skill| PickerItem {
                    value: skill.name.clone(),
                    label: skill.name.clone(),
                    description: skill.description.clone(),
                    group: None,
                    emphasized: crate::skills::is_project_skill(skill.source),
                })
                .collect::<Vec<_>>(),
            '@' => ctx
                .mention_candidates
                .iter()
                .filter(|path| crate::skills::fuzzy_match(path, &state.query))
                .map(|path| PickerItem {
                    value: path.clone(),
                    label: path.clone(),
                    description: None,
                    group: None,
                    emphasized: false,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let selected = match &self.menu {
            Some(menu)
                if menu.query == state.query
                    && menu.trigger == state.trigger
                    && menu.items.first().map(|item| &item.value)
                        == items.first().map(|item| &item.value) =>
            {
                menu.selected.min(items.len().saturating_sub(1))
            }
            _ => 0,
        };
        self.menu = Some(ComposerMenu {
            trigger: state.trigger,
            start: state.start,
            query: state.query.clone(),
            items,
            selected,
        });
    }

    fn close_menu(&mut self) {
        self.menu = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &ComposerContext<'_>) -> ComposerEvent {
        if let Some(menu) = self.menu.clone()
            && !menu.items.is_empty()
            && let Some(event) = self.handle_menu_key(&menu, key, ctx)
        {
            return event;
        }
        if self.attaching.is_some() {
            return self.handle_attach_key(key);
        }
        match key.code {
            KeyCode::Char('a')
                if self.quick_replies && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                ComposerEvent::QuickReply {
                    text: "Yes, approved.".to_owned(),
                }
            }
            KeyCode::Char('c')
                if self.quick_replies && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                ComposerEvent::QuickReply {
                    text: "Continue.".to_owned(),
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.attaching = Some(String::new());
                ComposerEvent::Changed
            }
            KeyCode::Char(character) => {
                self.insert_char(character);
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Backspace => {
                self.backspace();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Delete => {
                self.delete_forward();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_char('\n');
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Enter => {
                let text = self.text.trim().to_owned();
                self.close_menu();
                ComposerEvent::Submit { text }
            }
            KeyCode::Left => {
                self.move_left();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Right => {
                self.move_right();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Up => {
                self.move_line(-1);
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Down => {
                self.move_line(1);
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Home => {
                self.move_home();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::End => {
                self.move_end();
                self.refresh_menu(ctx);
                ComposerEvent::Changed
            }
            KeyCode::Esc => ComposerEvent::Blur,
            KeyCode::Tab => ComposerEvent::Blur,
            _ => ComposerEvent::Changed,
        }
    }

    fn handle_menu_key(
        &mut self,
        menu: &ComposerMenu,
        key: KeyEvent,
        ctx: &ComposerContext<'_>,
    ) -> Option<ComposerEvent> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(open) = self.menu.as_mut() {
                    open.selected = (open.selected + 1).min(open.items.len().saturating_sub(1));
                }
                Some(ComposerEvent::Changed)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(open) = self.menu.as_mut() {
                    open.selected = open.selected.saturating_sub(1);
                }
                Some(ComposerEvent::Changed)
            }
            KeyCode::Enter | KeyCode::Tab => {
                let item = menu.items.get(menu.selected).cloned();
                let Some(item) = item else {
                    return Some(ComposerEvent::Changed);
                };
                let (next, caret) =
                    apply_completion(&self.text, &menu_as_trigger(menu), self.caret, &item.value);
                self.text = next;
                self.caret = caret;
                let trigger = menu.trigger;
                self.menu = None;
                self.refresh_menu(ctx);
                if trigger == '/' {
                    Some(ComposerEvent::PickedSkill { name: item.value })
                } else {
                    Some(ComposerEvent::Changed)
                }
            }
            KeyCode::Esc => {
                self.menu = None;
                Some(ComposerEvent::Changed)
            }
            _ => None,
        }
    }

    fn handle_attach_key(&mut self, key: KeyEvent) -> ComposerEvent {
        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(path) = self.attaching.as_mut() {
                    path.push(character);
                }
            }
            KeyCode::Backspace => {
                if let Some(path) = self.attaching.as_mut() {
                    path.pop();
                }
            }
            KeyCode::Enter => {
                if let Some(path) = self.attaching.take()
                    && !path.trim().is_empty()
                {
                    self.attachments.push(Attachment { path });
                }
            }
            KeyCode::Esc => self.attaching = None,
            _ => {}
        }
        ComposerEvent::Changed
    }

    pub fn remove_attachment(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
        }
    }

    fn insert_char(&mut self, character: char) {
        if self.caret > self.text.len() {
            self.caret = self.text.len();
        }
        self.text.insert(self.caret, character);
        self.caret += character.len_utf8();
    }

    fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let end = self.caret;
        let start = self.text[..end]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.replace_range(start..end, "");
        self.caret = start;
    }

    fn delete_forward(&mut self) {
        if self.caret >= self.text.len() {
            return;
        }
        let start = self.caret;
        let end = self.text[start..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| start + i)
            .unwrap_or(self.text.len());
        self.text.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        if self.caret == 0 {
            return;
        }
        self.caret = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    fn move_right(&mut self) {
        if self.caret >= self.text.len() {
            return;
        }
        let start = self.caret;
        self.caret = self.text[start..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| start + i)
            .unwrap_or(self.text.len());
    }

    fn move_line(&mut self, delta: isize) {
        let (line, col) = self.caret_position();
        let target_line = (line as isize + delta).max(0) as usize;
        if target_line == line {
            return;
        }
        let line_start = self.line_start(target_line);
        let line_end = self.line_end(target_line);
        let chars_in_line = self.text[line_start..line_end].chars().count();
        let mut col = col;
        if col > chars_in_line {
            col = chars_in_line;
        }
        self.caret = self.text[line_start..]
            .char_indices()
            .nth(col)
            .map(|(offset, _)| line_start + offset)
            .unwrap_or(line_end);
    }

    fn move_home(&mut self) {
        let (line, _) = self.caret_position();
        self.caret = self.line_start(line);
    }

    fn move_end(&mut self) {
        let (line, _) = self.caret_position();
        self.caret = self.line_end(line);
    }

    fn caret_position(&self) -> (usize, usize) {
        let before = &self.text[..self.caret];
        let line = before.matches('\n').count();
        let last_nl = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
        let col = before[last_nl..].chars().count();
        (line, col)
    }

    fn line_start(&self, line: usize) -> usize {
        let mut index = 0;
        for _ in 0..line {
            if let Some(offset) = self.text[index..].find('\n') {
                index += offset + 1;
            }
        }
        index
    }

    fn line_end(&self, line: usize) -> usize {
        let start = self.line_start(line);
        match self.text[start..].find('\n') {
            Some(offset) => start + offset,
            None => self.text.len(),
        }
    }

    /// The number of text rows the composer occupies: grows with content between a
    /// three-line floor and an eight-line cap suitable for the terminal layout.
    pub fn height(&self) -> u16 {
        let lines = self.text.split('\n').count() + usize::from(self.attaching.is_some());
        lines.clamp(3, 8) as u16
    }

    /// Render the composer card (textarea + attachment row + footer). The host draws
    /// the pill row below it.
    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: Theme,
        hitmap: &mut HitMap,
        z: u8,
    ) {
        let style = if self.focused {
            Style::default().fg(theme.palette.fg)
        } else {
            Style::default().fg(theme.palette.soft_fg)
        };
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(if self.focused {
                " COMPOSER "
            } else {
                " COMPOSER (i to type) "
            })
            .border_style(Style::default().fg(theme.palette.border));
        if self.focused {
            block = block.border_style(Style::default().fg(theme.palette.accent));
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (caret_line, caret_col) = self.caret_position();
        let visible = (area.height.saturating_sub(4)).clamp(1, 6);
        let scroll = caret_line.saturating_sub(visible as usize - 1);
        let hscroll = caret_col.saturating_sub(inner.width as usize - 1);

        let mut row = inner.y;
        for offset in 0..visible as usize {
            if row >= inner.bottom() {
                break;
            }
            let line = scroll + offset;
            let content = &self.text[self.line_start(line)..self.line_end(line)];
            let chars: Vec<char> = content.chars().collect();
            let from = hscroll.min(chars.len());
            let is_caret_line = line == caret_line && self.focused;
            let mut spans = vec![Span::raw(format!(
                " {}",
                chars[from..].iter().collect::<String>()
            ))];
            if is_caret_line {
                let caret_col_in_box = caret_col.saturating_sub(hscroll);
                let prefix: String = chars
                    [from..from + caret_col_in_box.min(chars.len().saturating_sub(from))]
                    .iter()
                    .collect();
                let caret_char = chars.get(from + caret_col_in_box).copied().unwrap_or(' ');
                spans = vec![
                    Span::raw(format!(" {prefix}")),
                    Span::styled(
                        caret_char.to_string(),
                        Style::default()
                            .fg(theme.palette.bg)
                            .bg(theme.palette.accent),
                    ),
                ];
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(inner.x, row, inner.width, 1),
            );
            row += 1;
        }

        // Attachment row + footer.
        if row < inner.bottom() {
            let footer = if let Some(path) = &self.attaching {
                Line::from(vec![
                    Span::styled(" attach path: ", Style::default().fg(theme.palette.accent)),
                    Span::raw(path.clone()),
                    Span::styled("▌", Style::default().fg(theme.palette.fg)),
                ])
            } else {
                let mut attach_spans = vec![Span::styled(
                    " [attach]",
                    if self.attachments.is_empty() {
                        Style::default().fg(theme.palette.soft_fg)
                    } else {
                        Style::default().fg(theme.palette.fg)
                    },
                )];
                for attachment in &self.attachments {
                    let mut name = attachment.path.clone();
                    name.truncate(name.len().min(20));
                    attach_spans.push(Span::styled(
                        format!(" {name} ×"),
                        Style::default().fg(theme.palette.soft_fg),
                    ));
                }
                Line::from(attach_spans)
            };
            frame.render_widget(
                Paragraph::new(footer).style(style),
                Rect::new(inner.x + 1, row, inner.width.saturating_sub(2), 1),
            );
            let mut cursor = inner.x + 1;
            hitmap.register(
                Rect::new(cursor, row, 9, 1),
                z + 1,
                HitAction::ComposerAttach,
            );
            cursor += 9;
            for (index, attachment) in self.attachments.iter().enumerate() {
                let mut name = attachment.path.clone();
                name.truncate(name.len().min(20));
                let name_len = (name.chars().count() + 2) as u16;
                hitmap.register(
                    Rect::new(cursor, row, name_len, 1),
                    z + 1,
                    HitAction::ComposerRemoveAttachment(index),
                );
                cursor += name_len;
            }
            row += 1;
        }
        if row < inner.bottom() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " Enter send · Shift+Enter newline · Ctrl+Enter send · / skills · Alt+A / Alt+C quick replies",
                    Style::default().fg(theme.palette.soft_fg),
                ))),
                Rect::new(inner.x + 1, row, inner.width.saturating_sub(2), 1),
            );
        }

        if let Some(menu) = &self.menu {
            render_menu_overlay(frame, menu, area, theme);
        }
    }
}

fn menu_as_trigger(menu: &ComposerMenu) -> TriggerState {
    TriggerState {
        trigger: menu.trigger,
        start: menu.start,
        query: menu.query.clone(),
    }
}

/// The `/`-or-`@` autocomplete dropdown, drawn over the composer's top edge.
fn render_menu_overlay(frame: &mut Frame<'_>, menu: &ComposerMenu, area: Rect, theme: Theme) {
    if menu.items.is_empty() {
        return;
    }
    let width = area.width.min(56);
    let height = (menu.items.len() as u16 + 2).min(10).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y.saturating_add(1),
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, item) in menu.items.iter().enumerate() {
        let selected = index == menu.selected;
        let style = if selected {
            Style::default()
                .fg(theme.palette.bg)
                .bg(theme.palette.accent)
        } else if item.emphasized {
            Style::default()
                .fg(theme.palette.fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.palette.fg)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {} ", if selected { ">" } else { " " }, item.label),
            style,
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{} {}", menu.trigger, menu.query)),
            )
            .style(
                Style::default()
                    .fg(theme.palette.fg)
                    .bg(theme.palette.surface),
            ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ComposerContext<'static> {
        ComposerContext {
            skills: &[],
            skill_usage: None,
            mention_candidates: &[],
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn typing_appends_at_the_caret_and_emits_changed() {
        let mut composer = Composer::default();
        composer.focus();
        assert_eq!(
            composer.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE), &ctx()),
            ComposerEvent::Changed
        );
        assert_eq!(composer.text, "h");
        composer.handle_key(key(KeyCode::Char('i'), KeyModifiers::NONE), &ctx());
        assert_eq!(composer.text, "hi");
        assert_eq!(composer.caret, 2);
    }

    #[test]
    fn plain_enter_submits_and_shift_enter_newlines() {
        let mut composer = Composer::default();
        composer.set_text("  ship it  ");
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), &ctx()),
            ComposerEvent::Submit {
                text: "ship it".to_owned()
            }
        );
        composer.set_text("a");
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT), &ctx()),
            ComposerEvent::Changed
        );
        assert_eq!(composer.text, "a\n");
    }

    #[test]
    fn ctrl_enter_and_alt_enter_also_send() {
        let mut composer = Composer::default();
        composer.set_text("go");
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL), &ctx()),
            ComposerEvent::Submit {
                text: "go".to_owned()
            }
        );
        composer.set_text("go");
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter, KeyModifiers::ALT), &ctx()),
            ComposerEvent::Submit {
                text: "go".to_owned()
            }
        );
    }

    #[test]
    fn alt_a_and_alt_c_fire_the_quick_replies() {
        let mut composer = Composer::default();
        assert_eq!(
            composer.handle_key(key(KeyCode::Char('a'), KeyModifiers::ALT), &ctx()),
            ComposerEvent::QuickReply {
                text: "Yes, approved.".to_owned()
            }
        );
        assert_eq!(
            composer.handle_key(key(KeyCode::Char('c'), KeyModifiers::ALT), &ctx()),
            ComposerEvent::QuickReply {
                text: "Continue.".to_owned()
            }
        );
    }

    #[test]
    fn esc_blurs() {
        let mut composer = Composer::default();
        assert_eq!(
            composer.handle_key(key(KeyCode::Esc, KeyModifiers::NONE), &ctx()),
            ComposerEvent::Blur
        );
    }

    #[test]
    fn backspace_removes_the_char_before_the_caret() {
        let mut composer = Composer::default();
        composer.set_text("ab");
        composer.caret = 1;
        composer.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE), &ctx());
        assert_eq!(composer.text, "b");
        assert_eq!(composer.caret, 0);
    }

    #[test]
    fn detect_trigger_scans_from_the_caret_to_the_nearest_word_boundary() {
        assert_eq!(
            detect_trigger("/om", 3),
            Some(TriggerState {
                trigger: '/',
                start: 0,
                query: "om".to_owned()
            })
        );
        assert_eq!(
            detect_trigger("hi /om", 6),
            Some(TriggerState {
                trigger: '/',
                start: 3,
                query: "om".to_owned()
            })
        );
        assert_eq!(
            detect_trigger("https://om", 11),
            None,
            "mid-word trigger stays inert"
        );
        assert_eq!(
            detect_trigger("user@host", 9),
            None,
            "mid-word @ stays inert"
        );
        assert_eq!(
            detect_trigger("write /om stuff", 14),
            None,
            "a space commits the token"
        );
        assert_eq!(detect_trigger("", 0), None);
    }

    #[test]
    fn the_slash_menu_opens_and_picking_completes_the_token() {
        let mut composer = Composer::default();
        composer.focus();
        let skills = vec![Skill {
            name: "om-fix".to_owned(),
            description: Some("apply the minimal fix".to_owned()),
            interactive: None,
            body: String::new(),
            path: "/skills/om-fix.md".to_owned(),
            source: coducktor_contract::SkillSource::Ai,
            team: None,
        }];
        let ctx = ComposerContext {
            skills: &skills,
            skill_usage: None,
            mention_candidates: &[],
        };
        composer.handle_key(key(KeyCode::Char('/'), KeyModifiers::NONE), &ctx);
        assert!(composer.menu.is_some(), "typing / opens the menu");
        assert_eq!(composer.menu.as_ref().unwrap().items.len(), 1);

        let event = composer.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(
            event,
            ComposerEvent::PickedSkill {
                name: "om-fix".to_owned()
            }
        );
        assert_eq!(composer.text, "/om-fix ");
        assert_eq!(composer.caret, 8);
        assert!(composer.menu.is_none());
    }

    #[test]
    fn the_mention_menu_filters_the_candidate_list() {
        let mut composer = Composer::default();
        let ctx = ComposerContext {
            skills: &[],
            skill_usage: None,
            mention_candidates: &["src/lib/skills.ts".to_owned(), "src/main.rs".to_owned()],
        };
        for character in "@src".chars() {
            composer.handle_key(key(KeyCode::Char(character), KeyModifiers::NONE), &ctx);
        }
        let menu = composer.menu.as_ref().unwrap();
        assert_eq!(menu.trigger, '@');
        assert_eq!(menu.items.len(), 2);

        // A '/' inside the token is a mid-word trigger: it commits the token and
        // closes the menu because the slash starts a new path-like token.
        for character in "/ma".chars() {
            composer.handle_key(key(KeyCode::Char(character), KeyModifiers::NONE), &ctx);
        }
        assert!(composer.menu.is_none());
    }

    #[test]
    fn attachments_are_added_by_path_and_removable() {
        let mut composer = Composer::default();
        composer.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL), &ctx());
        assert!(composer.attaching.is_some());
        for character in "shot.png".chars() {
            composer.handle_key(key(KeyCode::Char(character), KeyModifiers::NONE), &ctx());
        }
        composer.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), &ctx());
        assert_eq!(
            composer.attachments,
            vec![Attachment {
                path: "shot.png".to_owned()
            }]
        );
        assert!(composer.attaching.is_none());

        composer.remove_attachment(0);
        assert!(composer.attachments.is_empty());
    }

    #[test]
    fn caret_navigation_moves_within_multi_line_text() {
        let mut composer = Composer::default();
        composer.set_text("one\ntwo");
        composer.caret = 3;
        composer.handle_key(key(KeyCode::Down, KeyModifiers::NONE), &ctx());
        assert_eq!(
            composer.caret, 7,
            "down lands at the same column on the next line"
        );
        composer.handle_key(key(KeyCode::Up, KeyModifiers::NONE), &ctx());
        assert_eq!(composer.caret, 3);
        composer.handle_key(key(KeyCode::Home, KeyModifiers::NONE), &ctx());
        assert_eq!(composer.caret, 0, "home lands at the line start");
    }
}
