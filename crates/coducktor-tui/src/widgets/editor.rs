//! The IDE's editor widget: multi-line text state with a row/col caret,
//! syntax highlighting through the shared `Highlighter`, a right-aligned line-number
//! gutter and a scroll offset — the pieces `screens/ide/` composes into the editor pane.
//!
//! The widget owns NO policy: keys arrive from the screen's `handle_key`, text stays
//! byte-exact (the engine round-trips the draft verbatim — no newline normalization, which would
//! corrupt a CRLF file on save). The caret is modeled as
//! `(row, col)` with `col` in chars within the line, so movement never needs byte-index
//! bookkeeping across edits.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::diff::highlight::{HighlightSpan, Highlighter};
use crate::theme::Theme;

/// Cap on the gutter width — a line number wider than this (10⁶ lines) is absurd for the
/// IDE's 1 MB file cap and keeps the layout computation bounded.
const MAX_GUTTER_DIGITS: usize = 6;

#[derive(Debug, Clone, Default)]
pub struct Editor {
    pub text: String,
    /// Caret row — a 0-based index into the newline-split line list.
    pub row: usize,
    /// Caret column in CHARS within `row`, never past the row's char count.
    pub col: usize,
    /// First visible line; `ensure_caret_visible` clamps it to keep the caret on screen.
    pub scroll: usize,
    /// Column memory for vertical movement (vi-style): set on the first up/down and kept
    /// until any horizontal movement or edit, so a zigzag down a ragged column stays put.
    pub preferred_col: Option<usize>,
}

impl Editor {
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
        self.row = 0;
        self.col = 0;
        self.scroll = 0;
        self.preferred_col = None;
    }

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    pub fn line(&self, index: usize) -> &str {
        self.text.split('\n').nth(index).unwrap_or("")
    }

    /// Clamp the caret and scroll after an external text replacement — the draft survives
    /// a data refresh only when the content is unchanged, so this is mostly a no-op.
    pub fn sanitize(&mut self) {
        let lines = self.line_count();
        self.row = self.row.min(lines.saturating_sub(1));
        let row_len = self.line(self.row).chars().count();
        self.col = self.col.min(row_len);
        self.ensure_caret_visible(usize::MAX);
    }

    /// Keep the caret row inside `[scroll, scroll + viewport)`.
    pub fn ensure_caret_visible(&mut self, viewport: usize) {
        if viewport == 0 {
            return;
        }
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll.saturating_add(viewport) {
            self.scroll = self.row.saturating_add(1).saturating_sub(viewport);
        }
    }

    pub fn insert_char(&mut self, character: char) {
        self.preferred_col = None;
        let line = self.line(self.row);
        let mut bytes = line.to_owned();
        let at = self.char_col_to_byte(line, self.col);
        bytes.insert(at, character);
        self.replace_row(bytes);
        self.col += 1;
    }

    pub fn insert_newline(&mut self) {
        self.preferred_col = None;
        let lines: Vec<&str> = self.text.split('\n').collect();
        let line = lines.get(self.row).copied().unwrap_or("");
        let at = self.char_col_to_byte(line, self.col);
        let (before, after) = line.split_at(at);
        let mut next = Vec::with_capacity(lines.len() + 1);
        next.extend_from_slice(&lines[..self.row]);
        next.push(before);
        next.push(after);
        next.extend_from_slice(&lines[self.row + 1..]);
        self.text = next.join("\n");
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        self.preferred_col = None;
        if self.col > 0 {
            let line = self.line(self.row);
            let at = self.char_col_to_byte(line, self.col);
            let mut bytes = line.to_owned();
            let start = bytes[..at]
                .chars()
                .next_back()
                .map(|c| at - c.len_utf8())
                .unwrap_or(0);
            bytes.replace_range(start..at, "");
            self.replace_row(bytes);
            self.col -= 1;
        } else if self.row > 0 {
            let joined = format!("{}{}", self.line(self.row - 1), self.line(self.row));
            let previous_len = self.line(self.row - 1).chars().count();
            let lines: Vec<&str> = self.text.split('\n').collect();
            let mut next = Vec::with_capacity(lines.len() - 1);
            next.extend_from_slice(&lines[..self.row - 1]);
            next.push(&joined);
            next.extend_from_slice(&lines[self.row + 1..]);
            self.text = next.join("\n");
            self.row -= 1;
            self.col = previous_len;
        }
    }

    pub fn delete_forward(&mut self) {
        self.preferred_col = None;
        let lines: Vec<&str> = self.text.split('\n').collect();
        let line = lines.get(self.row).copied().unwrap_or("");
        if self.col < line.chars().count() {
            let at = self.char_col_to_byte(line, self.col);
            let mut bytes = line.to_owned();
            let end = at + bytes[at..].chars().next().map(char::len_utf8).unwrap_or(0);
            bytes.replace_range(at..end, "");
            self.replace_row(bytes);
        } else if self.row + 1 < lines.len() {
            let joined = format!("{line}{}", lines[self.row + 1]);
            let mut next = Vec::with_capacity(lines.len() - 1);
            next.extend_from_slice(&lines[..self.row]);
            next.push(&joined);
            next.extend_from_slice(&lines[self.row + 2..]);
            self.text = next.join("\n");
        }
    }

    pub fn move_left(&mut self) {
        self.preferred_col = None;
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line(self.row).chars().count();
        }
    }

    pub fn move_right(&mut self) {
        self.preferred_col = None;
        let lines: Vec<&str> = self.text.split('\n').collect();
        let line = lines.get(self.row).copied().unwrap_or("");
        if self.col < line.chars().count() {
            self.col += 1;
        } else if self.row + 1 < lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.row > 0 {
            let preferred = *self.preferred_col.get_or_insert(self.col);
            self.row -= 1;
            self.col = preferred.min(self.line(self.row).chars().count());
        }
    }

    pub fn move_down(&mut self) {
        if self.row + 1 < self.line_count() {
            let preferred = *self.preferred_col.get_or_insert(self.col);
            self.row += 1;
            self.col = preferred.min(self.line(self.row).chars().count());
        }
    }

    pub fn move_home(&mut self) {
        self.preferred_col = None;
        self.col = 0;
    }

    pub fn move_end(&mut self) {
        self.preferred_col = None;
        self.col = self.line(self.row).chars().count();
    }

    /// `delta` lines up (negative) or down (positive), clamping to the document.
    pub fn move_pages(&mut self, delta: i64, viewport: usize) {
        let page = viewport.max(1) as i64;
        let target = self.row as i64 + delta * page;
        self.row = target.clamp(0, self.line_count().saturating_sub(1) as i64) as usize;
        self.col = self
            .preferred_col
            .unwrap_or(self.col)
            .min(self.line(self.row).chars().count());
    }

    fn replace_row(&mut self, row: String) {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut next = Vec::with_capacity(lines.len());
        next.extend_from_slice(&lines[..self.row]);
        next.push(&row);
        next.extend_from_slice(&lines[self.row + 1..]);
        self.text = next.join("\n");
    }

    fn char_col_to_byte(&self, line: &str, col: usize) -> usize {
        line.char_indices()
            .nth(col)
            .map(|(index, _)| index)
            .unwrap_or(line.len())
    }

    fn caret_span(color: Option<Color>, text: impl Into<String>) -> Span<'static> {
        let style = color
            .map(|color| Style::default().fg(color))
            .unwrap_or_default()
            .add_modifier(Modifier::REVERSED);
        Span::styled(text.into(), style)
    }

    fn content_spans(
        content: &str,
        runs: Option<&[HighlightSpan]>,
        start: usize,
        end: usize,
        caret: Option<usize>,
    ) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut push_segment = |text: String, absolute_start: usize, color: Option<Color>| {
            if text.is_empty() {
                return;
            }
            let text_len = text.chars().count();
            if let Some(caret) = caret
                && caret >= absolute_start
                && caret < absolute_start + text_len
            {
                let before: String = text.chars().take(caret - absolute_start).collect();
                let caret_char = text
                    .chars()
                    .nth(caret - absolute_start)
                    .unwrap_or(' ')
                    .to_string();
                let after: String = text.chars().skip(caret - absolute_start + 1).collect();
                if !before.is_empty() {
                    spans.push(match color {
                        Some(color) => Span::styled(before, Style::default().fg(color)),
                        None => Span::raw(before),
                    });
                }
                spans.push(Self::caret_span(color, caret_char));
                if !after.is_empty() {
                    spans.push(match color {
                        Some(color) => Span::styled(after, Style::default().fg(color)),
                        None => Span::raw(after),
                    });
                }
            } else {
                spans.push(match color {
                    Some(color) => Span::styled(text, Style::default().fg(color)),
                    None => Span::raw(text),
                });
            }
        };

        match runs {
            Some(runs) if !runs.is_empty() => {
                let mut run_start = 0;
                for run in runs {
                    let run_len = run.text.chars().count();
                    let run_end = run_start + run_len;
                    let clip_start = start.max(run_start);
                    let clip_end = end.min(run_end);
                    if clip_start < clip_end {
                        let text: String = run
                            .text
                            .chars()
                            .skip(clip_start - run_start)
                            .take(clip_end - clip_start)
                            .collect();
                        push_segment(text, clip_start, Some(run.color));
                    }
                    run_start = run_end;
                }
            }
            _ => {
                push_segment(
                    content
                        .chars()
                        .skip(start)
                        .take(end.saturating_sub(start))
                        .collect(),
                    start,
                    None,
                );
            }
        }

        if let Some(caret) = caret
            && caret == end
            && end == content.chars().count()
        {
            spans.push(Self::caret_span(None, " "));
        }
        spans
    }

    /// One rendered viewport row: gutter + highlighted content spans, with the caret cell
    /// reversed when this row is the focused caret row.
    fn render_row(
        &self,
        index: usize,
        span: Option<&[HighlightSpan]>,
        gutter_width: usize,
        theme: &Theme,
        focused: bool,
        horizontal_scroll: usize,
    ) -> Line<'static> {
        let gutter = format!("{:>width$}", index + 1, width = gutter_width);
        let mut spans = vec![Span::styled(
            gutter,
            Style::default().fg(theme.palette.soft_fg),
        )];
        spans.push(Span::raw(" "));
        let content = self.line(index);
        let content_len = content.chars().count();
        let start = horizontal_scroll.min(content_len);
        let caret = (focused && index == self.row).then_some(self.col.min(content_len));
        spans.extend(Self::content_spans(
            content,
            span,
            start,
            content_len,
            caret,
        ));
        Line::from(spans)
    }

    /// Render the whole editor viewport. The caller owns the `Highlighter` (one per screen);
    /// `None` highlight results (over the line cap) degrade to plain text — the same honesty
    /// the diff widget uses.
    pub fn render_lines(
        &self,
        path: &str,
        highlighter: &Highlighter,
        theme: &Theme,
        viewport: usize,
        focused: bool,
    ) -> Vec<Line<'static>> {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let gutter_width = lines
            .len()
            .min(10usize.pow(MAX_GUTTER_DIGITS as u32))
            .to_string()
            .len()
            .max(1);
        let owned: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        let highlighted = highlighter.highlight_lines(path, &owned);
        lines
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(viewport)
            .map(|(index, _)| {
                let spans = highlighted
                    .as_ref()
                    .and_then(|all| all.get(index))
                    .map(|runs| runs.as_slice());
                self.render_row(index, spans, gutter_width, theme, focused, 0)
            })
            .collect()
    }

    /// Render a soft-wrapped viewport for notes. The stored text keeps its logical newlines, but
    /// long lines occupy additional terminal rows so the caret and newly typed text remain
    /// visible instead of running past the right edge.
    pub fn render_wrapped_lines(
        &self,
        path: &str,
        highlighter: &Highlighter,
        theme: &Theme,
        viewport: usize,
        width: u16,
        focused: bool,
    ) -> Vec<Line<'static>> {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let gutter_width = lines
            .len()
            .min(10usize.pow(MAX_GUTTER_DIGITS as u32))
            .to_string()
            .len()
            .max(1);
        let content_width = usize::from(width).saturating_sub(gutter_width + 1).max(1);
        let owned: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        let highlighted = highlighter.highlight_lines(path, &owned);
        let mut rows = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let length = line.chars().count();
            let mut start = 0;
            loop {
                let end = (start + content_width).min(length);
                rows.push((index, start, end));
                if end == length {
                    if length > 0 && length % content_width == 0 {
                        rows.push((index, length, length));
                    }
                    break;
                }
                start = end;
            }
        }

        let caret_line = self.row.min(lines.len().saturating_sub(1));
        let caret_col = self.col.min(lines[caret_line].chars().count());
        let caret_row = rows
            .iter()
            .position(|(index, start, end)| {
                *index == caret_line
                    && caret_col >= *start
                    && (caret_col < *end
                        || (caret_col == *end && *end == lines[*index].chars().count()))
            })
            .unwrap_or(0);
        let first = if focused {
            caret_row.saturating_sub(viewport.saturating_sub(1))
        } else {
            0
        };

        rows.into_iter()
            .skip(first)
            .take(viewport)
            .map(|(index, start, end)| {
                let gutter = if start == 0 {
                    format!("{:>width$}", index + 1, width = gutter_width)
                } else {
                    " ".repeat(gutter_width)
                };
                let mut spans = vec![Span::styled(
                    gutter,
                    Style::default().fg(theme.palette.soft_fg),
                )];
                spans.push(Span::raw(" "));
                let line = lines[index];
                let highlighted_line = highlighted
                    .as_ref()
                    .and_then(|all| all.get(index))
                    .map(|runs| runs.as_slice());
                let caret = (focused && index == caret_line).then_some(caret_col);
                spans.extend(Self::content_spans(
                    line,
                    highlighted_line,
                    start,
                    end,
                    caret,
                ));
                Line::from(spans)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut editor = Editor::default();
        editor.set_text(text);
        editor
    }

    #[test]
    fn typing_inserts_at_the_caret_and_moves_it() {
        let mut editor = editor("ab\ncd");
        editor.row = 1;
        editor.col = 1;
        editor.insert_char('X');
        assert_eq!(editor.text, "ab\ncXd");
        assert_eq!(editor.col, 2);
    }

    #[test]
    fn newline_splits_the_row_and_keeps_everything_else() {
        let mut editor = editor("ab\ncd\nef");
        editor.row = 1;
        editor.col = 1;
        editor.insert_newline();
        assert_eq!(editor.text, "ab\nc\nd\nef");
        assert_eq!(editor.row, 2);
        assert_eq!(editor.col, 0);
    }

    #[test]
    fn backspace_joins_lines_at_the_row_head() {
        let mut editor = editor("ab\ncd");
        editor.row = 1;
        editor.col = 0;
        editor.backspace();
        assert_eq!(editor.text, "abcd");
        assert_eq!(editor.row, 0);
        assert_eq!(editor.col, 2);
    }

    #[test]
    fn backspace_removes_the_char_before_the_caret() {
        let mut editor = editor("héllo");
        editor.col = 2; // after the é
        editor.backspace();
        assert_eq!(editor.text, "hllo");
        assert_eq!(editor.col, 1);
    }

    #[test]
    fn delete_forward_joins_at_the_line_end() {
        let mut editor = editor("ab\ncd");
        editor.row = 0;
        editor.col = 2;
        editor.delete_forward();
        assert_eq!(editor.text, "abcd");
        assert_eq!(editor.row, 0);
        assert_eq!(editor.col, 2);
    }

    #[test]
    fn arrow_keys_wrap_around_line_ends() {
        let mut editor = editor("ab\ncd");
        editor.row = 0;
        editor.col = 0;
        editor.move_left();
        assert_eq!((editor.row, editor.col), (0, 0));
        editor.move_right();
        editor.move_right();
        editor.move_right(); // past "ab" → down to "cd" col 0
        assert_eq!((editor.row, editor.col), (1, 0));
        editor.move_right();
        editor.move_right();
        editor.move_right(); // stuck at end
        assert_eq!((editor.row, editor.col), (1, 2));
        editor.move_left();
        editor.move_left();
        editor.move_left(); // up to "ab" end
        assert_eq!((editor.row, editor.col), (0, 2));
    }

    #[test]
    fn vertical_movement_clamps_the_column_to_the_line() {
        let mut editor = editor("abcdef\nx");
        editor.row = 0;
        editor.col = 5;
        editor.move_down();
        assert_eq!((editor.row, editor.col), (1, 1));
        editor.move_up();
        assert_eq!((editor.row, editor.col), (0, 5));
    }

    #[test]
    fn scroll_follows_the_caret() {
        let mut editor = editor("");
        editor.set_text(
            &(0..100)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        editor.row = 99;
        editor.col = 0;
        editor.ensure_caret_visible(20);
        assert_eq!(editor.scroll, 80);
        editor.row = 5;
        editor.ensure_caret_visible(20);
        assert_eq!(editor.scroll, 5);
    }

    #[test]
    fn rendering_marks_the_caret_cell_and_keeps_gutter_alignment() {
        let mut editor = editor("fn main() {}\n");
        editor.col = 3;
        let theme = Theme::new(
            crate::theme::ThemeName::Dark,
            crate::theme::ColorCapability::TrueColor,
        );
        let highlighter = Highlighter::new(true);
        let lines = editor.render_lines("lib.rs", &highlighter, &theme, 10, true);
        let rendered: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.starts_with("1 fn "), "got {rendered:?}");
        let reversed = lines[0]
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(reversed.map(|span| span.content.as_ref()), Some("m"));
    }

    #[test]
    fn wrapped_rendering_keeps_a_long_line_and_end_caret_visible() {
        let mut editor = editor("abcdefghijklmno");
        editor.col = editor.text.chars().count();
        let theme = Theme::new(
            crate::theme::ThemeName::Dark,
            crate::theme::ColorCapability::TrueColor,
        );
        let highlighter = Highlighter::new(true);
        let lines = editor.render_wrapped_lines("notes.md", &highlighter, &theme, 3, 8, true);
        let rendered: String = lines
            .last()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .unwrap_or_default();
        assert!(rendered.contains("no"));
        assert!(lines.last().is_some_and(|line| {
            line.spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::REVERSED))
        }));
    }

    #[test]
    fn sanitize_clamps_an_out_of_range_caret() {
        let mut editor = editor("ab");
        editor.row = 5;
        editor.col = 9;
        editor.sanitize();
        assert_eq!((editor.row, editor.col), (0, 2));
    }
}
