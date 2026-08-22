//! The run-end rule: one full-width line that says a run stopped, and how.
//!
//! Two places draw it — the thread dock (where it replaces the live-activity row the moment a
//! run reaches a terminal status) and the transcript (where it is mirrored as an item so
//! scrolling back through a multi-turn thread shows where each run terminated). Both go through
//! [`banner_line`] so the two markers stay one visual language.

use std::env;
use std::sync::OnceLock;

use coducktor_contract::RunStatus;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{ColorCapability, Theme};

/// The four ways a run can stop. Deliberately not `RunStatus`: `Queued`/`Running`/`Idle`/
/// `Waiting` are mid-flight states that must not draw a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Done,
    Failed,
    Cancelled,
    Review,
}

impl RunOutcome {
    pub fn from_status(status: RunStatus) -> Option<Self> {
        match status {
            RunStatus::Done => Some(Self::Done),
            RunStatus::Failed => Some(Self::Failed),
            RunStatus::Cancelled => Some(Self::Cancelled),
            RunStatus::Review => Some(Self::Review),
            RunStatus::Queued | RunStatus::Running | RunStatus::Idle | RunStatus::Waiting => None,
        }
    }

    /// Identical geometry across all four outcomes is what builds recognition: the eye learns
    /// "the line means the run stopped", and the word tells it how afterwards.
    fn label(self) -> &'static str {
        match self {
            Self::Done => "RUN COMPLETE",
            Self::Failed => "RUN FAILED",
            Self::Cancelled => "RUN CANCELLED",
            Self::Review => "PAUSED FOR REVIEW",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Done => "\u{2713}",
            Self::Failed => "\u{00d7}",
            Self::Cancelled => "\u{2298}",
            Self::Review => "\u{25c6}",
        }
    }

    /// The ASCII stand-in for terminals that cannot draw the glyphs. Shorter on purpose — the
    /// bracket is doing the work the glyph does above.
    fn ascii_label(self) -> &'static str {
        match self {
            Self::Done => "[DONE]",
            Self::Failed => "[FAILED]",
            Self::Cancelled => "[CANCELLED]",
            Self::Review => "[REVIEW]",
        }
    }

    fn color(self, theme: &Theme) -> ratatui::style::Color {
        match self {
            Self::Done => theme.palette.done,
            Self::Failed => theme.palette.failed,
            Self::Cancelled => theme.palette.cancelled,
            Self::Review => theme.palette.review,
        }
    }
}

/// Whether the terminal's locale advertises UTF-8. Only the box-drawing and geometric glyphs
/// (`─ ✓ × ⊘ ◆`) degrade on a `false`; the `·` separator is Latin-1 and stays either way.
fn unicode_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|key| env::var(key).ok().filter(|value| !value.is_empty()))
            .is_some_and(|value| value.to_ascii_lowercase().contains("utf"))
    })
}

/// `───── ✓ RUN COMPLETE · 4m12s · 18.2k tok ─────`, centered in `width`.
///
/// Degrades on two independent axes: non-UTF-8 locales swap the rule and glyph for `=` and a
/// bracketed word, and anything below true color drops the hue for bold-only styling. The
/// shape survives both, which is the part that matters.
pub fn banner_line(outcome: RunOutcome, detail: &str, width: u16, theme: &Theme) -> Line<'static> {
    let unicode = unicode_supported();
    let rule_char = if unicode { '\u{2500}' } else { '=' };
    let mut label = if unicode {
        format!("{} {}", outcome.glyph(), outcome.label())
    } else {
        outcome.ascii_label().to_owned()
    };
    if !detail.is_empty() {
        label.push_str(" \u{b7} ");
        label.push_str(detail);
    }

    let colored = theme.capability == ColorCapability::TrueColor;
    let base = if colored {
        Style::default().fg(outcome.color(theme))
    } else {
        Style::default()
    };
    let label_span = Span::styled(label, base.add_modifier(Modifier::BOLD));
    let rule_style = base.add_modifier(Modifier::DIM);

    // Below this the rule stubs stop reading as a rule, so spend every column on the word.
    let label_width = label_span.width() as u16;
    if width < label_width + 4 {
        return Line::from(label_span);
    }
    let padding = width - label_width - 2;
    let left = padding / 2;
    let right = padding - left;
    Line::from(vec![
        Span::styled(rule_char.to_string().repeat(left as usize), rule_style),
        Span::raw(" "),
        label_span,
        Span::raw(" "),
        Span::styled(rule_char.to_string().repeat(right as usize), rule_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeName;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn every_outcome_gets_the_same_geometry_at_the_same_width() {
        let theme = Theme::new(ThemeName::Dark, ColorCapability::TrueColor);
        for outcome in [
            RunOutcome::Done,
            RunOutcome::Failed,
            RunOutcome::Cancelled,
            RunOutcome::Review,
        ] {
            let line = banner_line(outcome, "4m12s \u{b7} 18.2k tok", 80, &theme);
            assert_eq!(line.width(), 80, "{outcome:?} fills the width");
            assert_eq!(line.spans.len(), 5, "{outcome:?} is rule/label/rule");
        }
    }

    #[test]
    fn mid_flight_statuses_have_no_outcome() {
        assert_eq!(RunOutcome::from_status(RunStatus::Running), None);
        assert_eq!(RunOutcome::from_status(RunStatus::Idle), None);
        assert_eq!(
            RunOutcome::from_status(RunStatus::Done),
            Some(RunOutcome::Done)
        );
    }

    #[test]
    fn a_narrow_dock_keeps_the_word_and_drops_the_rule() {
        let theme = Theme::new(ThemeName::Dark, ColorCapability::TrueColor);
        let line = banner_line(RunOutcome::Review, "", 8, &theme);
        assert_eq!(line.spans.len(), 1);
        assert!(line_text(&line).contains("PAUSED FOR REVIEW"));
    }

    #[test]
    fn below_truecolor_the_banner_is_bold_only() {
        let theme = Theme::new(ThemeName::Dark, ColorCapability::Ansi16);
        let line = banner_line(RunOutcome::Done, "12s", 60, &theme);
        assert!(
            line.spans.iter().all(|span| span.style.fg.is_none()),
            "no hue survives below true color"
        );
        assert!(
            line.spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
            "the label is still bold"
        );
    }
}
