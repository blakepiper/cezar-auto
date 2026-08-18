//! Shared grouped task-card list used by project Tasks and workspace All Tasks.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::input::hitmap::{HitAction, HitMap};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CardGroup {
    NeedsYou,
    Working,
    Recent,
    Archived,
}

impl CardGroup {
    fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "NEEDS YOU",
            Self::Working => "WORKING",
            Self::Recent => "RECENT",
            Self::Archived => "ARCHIVED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskCard {
    pub key: String,
    pub group: CardGroup,
    pub glyph: &'static str,
    pub status: &'static str,
    pub title: String,
    pub prompt: String,
    pub activity: String,
    pub project: Option<String>,
    pub metadata: Vec<String>,
    pub unread: bool,
}

/// Render whole cards only. `scroll` is a card index, which keeps selection and scrolling stable
/// even when prompt wrapping changes between terminal sizes.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    cards: &[TaskCard],
    selected: Option<usize>,
    scroll: &mut usize,
    hitmap: &mut HitMap,
    title: &str,
    empty_hint: &str,
    theme: &Theme,
) {
    let outer = Block::default().borders(Borders::ALL).title(title);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    if cards.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_hint).style(Style::default().fg(theme.palette.soft_fg)),
            inner,
        );
        *scroll = 0;
        return;
    }

    if let Some(selected) = selected {
        if selected < *scroll {
            *scroll = selected;
        }
        let visible = usize::from(inner.height / 6).max(1);
        if selected >= scroll.saturating_add(visible) {
            *scroll = selected + 1 - visible;
        }
    }
    *scroll = (*scroll).min(cards.len().saturating_sub(1));

    let mut y = inner.y;
    let mut previous_group = None;
    for (index, card) in cards.iter().enumerate().skip(*scroll) {
        if previous_group != Some(card.group) {
            if y >= inner.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", card.group.label()),
                    Style::default()
                        .fg(theme.palette.soft_fg)
                        .add_modifier(Modifier::BOLD),
                ))),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
            previous_group = Some(card.group);
        }
        if y >= inner.bottom() {
            break;
        }
        let height = 6.min(inner.bottom() - y);
        let card_area = Rect::new(inner.x, y, inner.width, height);
        let is_selected = selected == Some(index);
        let border = if is_selected {
            theme.palette.accent
        } else {
            theme.palette.border
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border));
        let content = block.inner(card_area);
        frame.render_widget(block, card_area);

        if content.height > 0 {
            let marker = if is_selected { "▶" } else { " " };
            let title_style = if card.unread || is_selected {
                Style::default()
                    .fg(theme.palette.fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.palette.fg)
            };
            let mut header = vec![
                Span::styled(
                    format!("{marker} {} {}  ", card.glyph, card.status),
                    title_style,
                ),
                Span::styled(card.title.clone(), title_style),
            ];
            if let Some(project) = &card.project {
                header.push(Span::styled(
                    format!("  {project}"),
                    Style::default().fg(theme.palette.accent),
                ));
            }
            header.push(Span::styled(
                format!("  {}", card.activity),
                Style::default().fg(theme.palette.soft_fg),
            ));
            frame.render_widget(
                Paragraph::new(Line::from(header)),
                Rect::new(content.x, content.y, content.width, 1),
            );
        }
        if content.height > 1 {
            frame.render_widget(
                Paragraph::new(card.prompt.clone())
                    .style(Style::default().fg(theme.palette.soft_fg))
                    .wrap(Wrap { trim: false }),
                Rect::new(
                    content.x + 2,
                    content.y + 1,
                    content.width.saturating_sub(2),
                    2,
                ),
            );
        }
        if content.height > 3 && !card.metadata.is_empty() {
            frame.render_widget(
                Paragraph::new(card.metadata.join("  ·  "))
                    .style(Style::default().fg(theme.palette.soft_fg)),
                Rect::new(
                    content.x + 2,
                    content.y + 3,
                    content.width.saturating_sub(2),
                    1,
                ),
            );
        }
        hitmap.register(card_area, 2, HitAction::TableRow(index));
        y = y.saturating_add(height);
    }
}
