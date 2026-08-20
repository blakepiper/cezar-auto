//! Syntax highlighting for the diff widget: `syntect` + `two-face`'s bundled
//! `bat` syntax/theme assets. This is the one highlighter
//! instance the diff widget draws colors from — but there is no cross-widget singleton
//! requirement in the TUI the way there was for the Shiki instance shared with chat code
//! blocks, since markdown code fences render through `tui-markdown`, not through this
//! module.

use ratatui::style::Color;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

/// Past this many lines a file skips syntax highlighting entirely — plaintext beats jank.
/// Maximum number of lines highlighted in one diff block.
pub const HIGHLIGHT_MAX_LINES: usize = 1500;

/// One highlighted run: text plus its resolved foreground color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub text: String,
    pub color: Color,
}

#[derive(Default)]
pub struct Highlighter;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static DARK_THEME: OnceLock<Theme> = OnceLock::new();
static LIGHT_THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_no_newlines)
}

fn theme(dark: bool) -> &'static Theme {
    let slot = if dark { &DARK_THEME } else { &LIGHT_THEME };
    slot.get_or_init(|| {
        let theme_set = two_face::theme::extra();
        theme_set
            .get(if dark {
                two_face::theme::EmbeddedThemeName::Nord
            } else {
                two_face::theme::EmbeddedThemeName::Github
            })
            .clone()
    })
}

impl Highlighter {
    pub fn new() -> Self {
        Self
    }

    /// Highlight a whole file's lines (already reassembled from hunk + expanded-gap text, in
    /// display order — see `render.rs`'s `line_list`) and return one span vec per input line.
    /// `None` when the file is too large or the language could not be resolved to anything
    /// beyond plain text (plain text still highlights, just with a single unstyled span, so
    /// this only returns `None` above the line cap). `dark` picks the syntax palette that
    /// matches the active theme's background so foreground colors stay legible.
    pub fn highlight_lines(
        &self,
        path: &str,
        lines: &[String],
        dark: bool,
    ) -> Option<Vec<Vec<HighlightSpan>>> {
        if lines.len() > HIGHLIGHT_MAX_LINES {
            return None;
        }
        let syntax_set = syntax_set();
        let theme = theme(dark);
        let extension = path.rsplit('.').next().unwrap_or("");
        let syntax = syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let mut with_newline = line.clone();
            with_newline.push('\n');
            let Ok(ranges) = highlighter.highlight_line(&with_newline, syntax_set) else {
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
        let highlighter = Highlighter::new();
        let lines = vec!["fn main() {}".to_owned()];
        let spans = highlighter
            .highlight_lines("lib.rs", &lines, true)
            .expect("small file highlights");
        assert_eq!(spans.len(), 1);
        assert!(!spans[0].is_empty());
    }

    #[test]
    fn a_file_past_the_line_cap_is_not_highlighted() {
        let highlighter = Highlighter::new();
        let lines: Vec<String> = (0..HIGHLIGHT_MAX_LINES + 1)
            .map(|i| format!("line {i}"))
            .collect();
        assert!(
            highlighter
                .highlight_lines("lib.rs", &lines, true)
                .is_none()
        );
    }

    #[test]
    fn unknown_extensions_degrade_to_plain_text_rather_than_failing() {
        let highlighter = Highlighter::new();
        let lines = vec!["whatever content".to_owned()];
        let spans = highlighter
            .highlight_lines("file.made-up-extension", &lines, true)
            .expect("plain text still highlights");
        assert_eq!(spans.len(), 1);
    }
}
