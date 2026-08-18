use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Paragraph};

use crate::theme::Theme;

const LOGO: &str = include_str!("../../../logo.txt");
const REVEAL_DURATION: Duration = Duration::from_millis(650);
const TOTAL_DURATION: Duration = Duration::from_millis(1_200);

/// The short opening splash shown once when the TUI starts.
pub struct WelcomeAnimation {
    started: Instant,
    skipped: bool,
}

impl WelcomeAnimation {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            skipped: false,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.skipped && self.started.elapsed() < TOTAL_DURATION
    }

    pub fn skip(&mut self) {
        self.skipped = true;
    }

    /// Consume startup input while the splash is visible. Quit and focus-navigation keys are
    /// forwarded so the corresponding application-level behavior remains available immediately.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        if !self.is_active() {
            return true;
        }
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            self.skip();
            return matches!(key.code, KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q'))
                || (key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Left | KeyCode::Right));
        }
        false
    }

    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let area = frame.area();
        let style = Style::default().bg(theme.palette.bg);
        frame.render_widget(Block::default().style(style), area);

        let lines = logo_lines();
        let visible = visible_line_count(self.started.elapsed(), lines.len());
        if visible == 0 || area.width == 0 || area.height == 0 {
            return;
        }

        let height = visible.min(usize::from(area.height));
        let start = visible.saturating_sub(height) / 2;
        let text = lines
            .iter()
            .skip(start)
            .take(height)
            .map(|line| (*line).into())
            .collect::<Vec<_>>();
        let y = area.y + area.height.saturating_sub(height as u16) / 2;
        let logo_area = Rect::new(area.x, y, area.width, height as u16);

        frame.render_widget(
            Paragraph::new(text).alignment(Alignment::Center).style(
                Style::default()
                    .fg(theme.palette.accent)
                    .bg(theme.palette.bg),
            ),
            logo_area,
        );
    }
}

impl Default for WelcomeAnimation {
    fn default() -> Self {
        Self::new()
    }
}

fn logo_lines() -> Vec<&'static str> {
    let lines = LOGO.lines().collect::<Vec<_>>();
    let Some(start) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Vec::new();
    };
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)
        .unwrap_or(start);
    lines[start..end].to_vec()
}

fn visible_line_count(elapsed: Duration, total: usize) -> usize {
    if elapsed >= REVEAL_DURATION {
        return total;
    }
    let elapsed_ms = elapsed.as_millis() as usize;
    let duration_ms = REVEAL_DURATION.as_millis() as usize;
    total.saturating_mul(elapsed_ms) / duration_ms
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;

    #[test]
    fn embeds_the_repository_logo_without_outer_padding() {
        let lines = logo_lines();
        assert!(!lines.is_empty());
        assert!(lines.first().is_some_and(|line| !line.trim().is_empty()));
        assert!(lines.last().is_some_and(|line| !line.trim().is_empty()));
    }

    #[test]
    fn reveal_progress_starts_empty_and_ends_complete() {
        let total = logo_lines().len();
        assert_eq!(visible_line_count(Duration::ZERO, total), 0);
        assert_eq!(visible_line_count(REVEAL_DURATION, total), total);
    }

    #[test]
    fn startup_keys_skip_the_splash_and_forward_quit_or_focus_navigation() {
        let mut animation = WelcomeAnimation::new();
        assert!(!animation.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            crossterm::event::KeyModifiers::NONE,
        ))));
        assert!(!animation.is_active());

        let mut animation = WelcomeAnimation::new();
        assert!(animation.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        ))));
        assert!(!animation.is_active());

        let mut animation = WelcomeAnimation::new();
        assert!(animation.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::CONTROL,
        ))));
        assert!(!animation.is_active());
    }
}
