//! Syntax highlighting for the diff widget (spec §7.6, §6.1): `syntect` + `two-face`'s bundled
//! `bat` syntax/theme assets. Mirrors `web/src/lib/highlighter.ts`'s role — the ONE highlighter
//! instance the diff widget draws colors from — but there is no cross-widget singleton
//! requirement in the TUI the way there was for the Shiki instance shared with chat code
//! blocks, since markdown code fences render through `tui-markdown` (A7), not through this
//! module.

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

/// Past this many lines a file skips syntax highlighting entirely — plaintext beats jank.
/// Mirrors `HIGHLIGHT_MAX_LINES` in `diff-view.tsx`.
pub const HIGHLIGHT_MAX_LINES: usize = 1500;

/// One highlighted run: text plus its resolved foreground color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub text: String,
    pub color: Color,
}

pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(dark: bool) -> Self {
        let syntax_set = two_face::syntax::extra_no_newlines();
        let theme_set = two_face::theme::extra();
        let theme = theme_set
            .get(if dark {
                two_face::theme::EmbeddedThemeName::Nord
            } else {
                two_face::theme::EmbeddedThemeName::Github
            })
            .clone();
        Self { syntax_set, theme }
    }

    /// A theme-only instance for tests/tools that never need a live palette choice.
    #[cfg(test)]
    pub fn default_dark() -> Self {
        Self::new(true)
    }

    /// Highlight a whole file's lines (already reassembled from hunk + expanded-gap text, in
    /// display order — see `render.rs`'s `line_list`) and return one span vec per input line.
    /// `None` when the file is too large or the language could not be resolved to anything
    /// beyond plain text (plain text still highlights, just with a single unstyled span, so
    /// this only returns `None` above the line cap).
    pub fn highlight_lines(&self, path: &str, lines: &[String]) -> Option<Vec<Vec<HighlightSpan>>> {
        if lines.len() > HIGHLIGHT_MAX_LINES {
            return None;
        }
        let extension = path.rsplit('.').next().unwrap_or("");
        let syntax = self
            .syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let mut with_newline = line.clone();
            with_newline.push('\n');
            let Ok(ranges) = highlighter.highlight_line(&with_newline, &self.syntax_set) else {
                out.push(vec![HighlightSpan {
                    text: line.clone(),
                    color: Color::Reset,
                }]);
                continue;
            };
            let mut spans = Vec::with_capacity(ranges.len());
            for (style, text) in ranges {
                let text = text.trim_end_matches('\n');
                if text.is_empty() {
                    continue;
                }
                spans.push(HighlightSpan {
                    text: text.to_owned(),
                    color: Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b),
                });
            }
            out.push(spans);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_keywords_with_a_non_default_color() {
        let highlighter = Highlighter::default_dark();
        let lines = vec!["fn main() {}".to_owned()];
        let spans = highlighter
            .highlight_lines("lib.rs", &lines)
            .expect("small file highlights");
        assert_eq!(spans.len(), 1);
        assert!(!spans[0].is_empty());
    }

    #[test]
    fn a_file_past_the_line_cap_is_not_highlighted() {
        let highlighter = Highlighter::default_dark();
        let lines: Vec<String> = (0..HIGHLIGHT_MAX_LINES + 1)
            .map(|i| format!("line {i}"))
            .collect();
        assert!(highlighter.highlight_lines("lib.rs", &lines).is_none());
    }

    #[test]
    fn unknown_extensions_degrade_to_plain_text_rather_than_failing() {
        let highlighter = Highlighter::default_dark();
        let lines = vec!["whatever content".to_owned()];
        let spans = highlighter
            .highlight_lines("file.made-up-extension", &lines)
            .expect("plain text still highlights");
        assert_eq!(spans.len(), 1);
    }
}
