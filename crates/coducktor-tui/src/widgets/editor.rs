//! The IDE's editor widget (spec §8.8): multi-line text state with a row/col caret,
//! syntax highlighting through the shared `Highlighter`, a right-aligned line-number
//! gutter and a scroll offset — the pieces `screens/ide/` composes into the editor pane.
//!
//! The widget owns NO policy: keys arrive from the screen's `handle_key`, text stays
//! byte-exact (the server's `PUT /ide/file` round-trips the draft verbatim — no newline
//! normalization, which would corrupt a CRLF file on save). The caret is modeled as
//! `(row, col)` with `col` in chars within the line, so movement never needs byte-index
//! bookkeeping across edits.

use ratatui::style::{Modifier, Style};
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
    /// a server refetch only when the content is unchanged, so this is mostly a no-op.
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

    /// One rendered viewport row: gutter + highlighted content spans, with the caret cell
    /// reversed when this row is the focused caret row.
    fn render_row(
        &self,
        index: usize,
        span: Option<&[HighlightSpan]>,
        gutter_width: usize,
        theme: &Theme,
        focused: bool,
    ) -> Line<'static> {
        let gutter = format!("{:>width$}", index + 1, width = gutter_width);
        let mut spans = vec![Span::styled(
            gutter,
            Style::default().fg(theme.palette.soft_fg),
        )];
        spans.push(Span::raw(" "));
        let content = self.line(index);
        let mut col = 0usize;
        let caret_here = focused && index == self.row;
        match span {
            Some(runs) => {
                for run in runs {
                    let text = run.text.as_str();
                    let span_end = col + text.chars().count();
                    if caret_here && span_end > self.col && self.col >= col {
                        let split = self.col - col;
                        let split_text: String = text.chars().take(split).collect();
                        let split_byte = split_text.len();
                        let caret_char_len = text[split_byte..]
                            .chars()
                            .next()
                            .map(char::len_utf8)
                            .unwrap_or(0);
                        let (before, rest) = text.split_at(split_byte);
                        if !before.is_empty() {
                            spans.push(Span::styled(
                                before.to_owned(),
                                Style::default().fg(run.color),
                            ));
                        }
                        if let Some(caret_char) = rest.get(..caret_char_len) {
                            spans.push(Span::styled(
                                caret_char.to_owned(),
                                Style::default()
                                    .fg(run.color)
                                    .add_modifier(Modifier::REVERSED),
                            ));
                        }
                        let tail = &rest[caret_char_len..];
                        if !tail.is_empty() {
                            spans.push(Span::styled(
                                tail.to_owned(),
                                Style::default().fg(run.color),
                            ));
                        }
                        col = span_end;
                        continue;
                    }
                    spans.push(Span::styled(
                        text.to_owned(),
                        Style::default().fg(run.color),
                    ));
                    col = span_end;
                }
            }
            None => {
                if caret_here {
                    let caret = content
                        .chars()
                        .nth(self.col)
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| " ".to_owned());
                    spans.push(Span::styled(
                        caret,
                        Style::default().add_modifier(Modifier::REVERSED),
                    ));
                } else {
                    spans.push(Span::raw(content.to_owned()));
                }
            }
        }
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
                self.render_row(index, spans, gutter_width, theme, focused)
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
    fn sanitize_clamps_an_out_of_range_caret() {
        let mut editor = editor("ab");
        editor.row = 5;
        editor.col = 9;
        editor.sanitize();
        assert_eq!((editor.row, editor.col), (0, 2));
    }
}
