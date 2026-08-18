//! A centered overlay list picker — the shared surface behind every new-task pill.
//!
//! Two flavors, one widget:
//! - the *searchable grouped* picker (the skill/workflow source pill): a search line
//!   plus items carrying a group heading and an "emphasized" (project-skill) flag;
//! - the *simple radio* picker (runner, model, reasoning, variants, base, account):
//!   no search line, one flat list.
//!
//! The screen owns the candidate list: it recomputes the items whenever the query
//! changes (via the `skills`/`new_task_form` ranking ports) and hands the widget
//! only the already-ordered `PickerItem`s.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::input::hitmap::{HitAction, HitMap};
use crate::theme::Theme;

/// One row of an open picker, in display (group) order.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
    pub group: Option<String>,
    /// Project skills render emphasized (bold) wherever skills are listed.
    pub emphasized: bool,
}

impl PickerItem {
    pub fn simple(
        value: impl Into<String>,
        label: impl Into<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description,
            group: None,
            emphasized: false,
        }
    }
}

/// The keyboard contract of an open picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEvent {
    /// The query box gained a character (or a backspace) — the owner should refilter.
    Query(String),
    /// The user picked the item at this index.
    Select(usize),
    /// The user asked to close without picking.
    Close,
    /// Nothing for the owner to do.
    Noop,
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub title: String,
    pub query: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub searchable: bool,
}

impl Picker {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: String::new(),
            items: Vec::new(),
            selected: 0,
            searchable: true,
        }
    }

    /// Refresh the candidate list (e.g. after the query changed) and keep the
    /// selection inside it.
    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        let selected = self.selected.min(items.len().saturating_sub(1));
        self.items = items;
        self.selected = selected;
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> PickerEvent {
        use crossterm::event::KeyCode;
        if self.searchable {
            match key.code {
                KeyCode::Char(character) => {
                    self.query.push(character);
                    return PickerEvent::Query(self.query.clone());
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    return PickerEvent::Query(self.query.clone());
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter => {
                if self.items.is_empty() {
                    PickerEvent::Close
                } else {
                    PickerEvent::Select(self.selected)
                }
            }
            KeyCode::Esc => PickerEvent::Close,
            _ => PickerEvent::Noop,
        }
    }

    fn move_selection(&mut self, delta: isize) -> PickerEvent {
        if self.items.is_empty() {
            return PickerEvent::Noop;
        }
        let next =
            (self.selected as isize + delta).clamp(0, self.items.len() as isize - 1) as usize;
        self.selected = next;
        PickerEvent::Noop
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: Theme, hitmap: &mut HitMap) {
        let width = area.width.min(64);
        let mut content_lines = if self.searchable { 1 } else { 0 };
        let mut index = 0;
        let mut previous_group: Option<&str> = None;
        while index < self.items.len() {
            let item = &self.items[index];
            if item.group.as_deref() != previous_group {
                content_lines += 1;
                previous_group = item.group.as_deref();
            }
            content_lines += 1;
            index += 1;
        }
        let height = (content_lines + 2).min(area.height.saturating_sub(2));
        let rect = centered_rect(area, width, height);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(self.title.clone())
                .style(
                    Style::default()
                        .fg(theme.palette.fg)
                        .bg(theme.palette.surface),
                ),
            rect,
        );
        let mut row = rect.y + 1;
        if self.searchable {
            let search = Span::styled(
                format!("search {}▌", self.query),
                Style::default().fg(theme.palette.soft_fg),
            );
            frame.render_widget(
                Paragraph::new(search),
                Rect::new(rect.x + 1, row, rect.width.saturating_sub(2), 1),
            );
            row += 1;
        }
        let mut index = 0;
        let mut previous_group: Option<&str> = None;
        while index < self.items.len() {
            if row >= rect.bottom().saturating_sub(1) {
                break;
            }
            let item = &self.items[index];
            if item.group.as_deref() != previous_group {
                if let Some(group) = &item.group {
                    let heading = Span::styled(
                        format!(" {} ", group),
                        Style::default()
                            .fg(theme.palette.soft_fg)
                            .add_modifier(Modifier::BOLD),
                    );
                    frame.render_widget(
                        Paragraph::new(heading),
                        Rect::new(rect.x + 1, row, rect.width.saturating_sub(2), 1),
                    );
                    row += 1;
                }
                previous_group = item.group.as_deref();
            }
            let selected = index == self.selected;
            let mut style = if selected {
                Style::default()
                    .fg(theme.palette.bg)
                    .bg(theme.palette.accent)
            } else {
                Style::default().fg(theme.palette.fg)
            };
            if item.emphasized && !selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            let marker = if selected { ">" } else { " " };
            let mut spans = vec![Span::styled(format!(" {marker} {} ", item.label), style)];
            if let Some(description) = &item.description {
                let desc = description.chars().take(40).collect::<String>();
                spans.push(Span::styled(
                    format!("  {desc}"),
                    Style::default().fg(theme.palette.soft_fg),
                ));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(rect.x + 1, row, rect.width.saturating_sub(2), 1),
            );
            hitmap.register(
                Rect::new(rect.x + 1, row, rect.width.saturating_sub(2), 1),
                10,
                HitAction::PickerRow(index),
            );
            row += 1;
            index += 1;
        }
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::input::hitmap::HitMap;
    use crate::theme::{ColorCapability, ThemeName};

    fn picker(items: Vec<PickerItem>) -> Picker {
        let mut picker = Picker::new("SOURCE");
        picker.set_items(items);
        picker
    }

    fn item(label: &str, group: Option<&str>) -> PickerItem {
        PickerItem {
            value: label.to_owned(),
            label: label.to_owned(),
            description: None,
            group: group.map(ToOwned::to_owned),
            emphasized: false,
        }
    }

    #[test]
    fn keys_move_selection_and_pick() {
        let mut picker = picker(vec![item("a", None), item("b", None)]);
        picker.searchable = false;
        assert_eq!(picker.handle_key(key_char('j')), PickerEvent::Noop);
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.handle_key(key_char('k')), PickerEvent::Noop);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.handle_key(enter_key()), PickerEvent::Select(0));
        assert_eq!(picker.handle_key(esc_key()), PickerEvent::Close);
    }

    #[test]
    fn query_keys_emit_query_events_for_searchable_pickers() {
        let mut picker = picker(vec![item("a", None)]);
        assert_eq!(
            picker.handle_key(key_char('x')),
            PickerEvent::Query("x".to_owned())
        );
        assert_eq!(picker.query, "x");
        assert_eq!(
            picker.handle_key(backspace_key()),
            PickerEvent::Query(String::new())
        );
    }

    #[test]
    fn non_searchable_pickers_keep_letter_keys_for_selection() {
        let mut picker = Picker::new("RUNNER");
        picker.searchable = false;
        picker.set_items(vec![item("a", None), item("b", None)]);
        assert_eq!(picker.handle_key(key_char('j')), PickerEvent::Noop);
        assert_eq!(picker.selected, 1);
    }

    #[test]
    fn selection_is_clamped_into_a_shrinking_list() {
        let mut picker = picker(vec![item("a", None), item("b", None), item("c", None)]);
        picker.searchable = false;
        picker.handle_key(key_char('j'));
        picker.handle_key(key_char('j'));
        assert_eq!(picker.selected, 2);
        picker.set_items(vec![item("x", None)]);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn selected_descriptions_use_a_readable_foreground() {
        let picker = picker(vec![PickerItem {
            value: "execution".to_owned(),
            label: "execution".to_owned(),
            description: Some("readable description".to_owned()),
            group: Some("task mode".to_owned()),
            emphasized: false,
        }]);
        let theme = Theme::new(ThemeName::LazyVim, ColorCapability::TrueColor);
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut hitmap = HitMap::default();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), theme, &mut hitmap))
            .unwrap();

        let description_start = terminal
            .backend()
            .buffer()
            .cell((24, 10))
            .expect("description starts at the expected cell");
        assert_eq!(description_start.symbol(), "r");
        assert_eq!(description_start.fg, theme.palette.soft_fg);
    }

    fn key_char(character: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        )
    }
    fn enter_key() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )
    }
    fn esc_key() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        )
    }
    fn backspace_key() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::NONE,
        )
    }
}
