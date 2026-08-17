//! Word-level intra-line diff for paired del/add lines. Dissimilar lines (a full rewrite) get
//! no word marks, because marking most of a line tells the reader nothing.

use similar::{ChangeTag, TextDiff};

/// One renderable run of a diff line's word-level marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordSpan {
    pub text: String,
    /// True for the tokens this side does not share with the other side.
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordDiff {
    pub del: Vec<WordSpan>,
    pub add: Vec<WordSpan>,
}

/// Past this many word-level changes per side the diff is not worth the noise — no marks.
const MAX_CHANGES: usize = 600;

/// Lines sharing fewer than this fraction of tokens are rewrites, not edits — no marks.
const MIN_SIMILARITY: f32 = 0.3;

/// Word spans for one del/add line pair, or `None` when marks would not help the reader
/// (identical lines, rewrites, or a degenerate/oversized input).
pub fn diff_words(before: &str, after: &str) -> Option<WordDiff> {
    if before == after {
        return None;
    }
    if before.is_empty() || after.is_empty() {
        return None;
    }
    let diff = TextDiff::from_words(before, after);
    if diff.ratio() < MIN_SIMILARITY {
        return None;
    }

    let mut del: Vec<WordSpan> = Vec::new();
    let mut add: Vec<WordSpan> = Vec::new();
    let mut changes = 0usize;
    for change in diff.iter_all_changes() {
        let changed = change.tag() != ChangeTag::Equal;
        if changed {
            changes += 1;
        }
        match change.tag() {
            ChangeTag::Equal => {
                push_span(&mut del, change.value(), false);
                push_span(&mut add, change.value(), false);
            }
            ChangeTag::Delete => push_span(&mut del, change.value(), true),
            ChangeTag::Insert => push_span(&mut add, change.value(), true),
        }
    }
    if changes > MAX_CHANGES {
        return None;
    }
    Some(WordDiff { del, add })
}

/// Collapse adjacent runs with the same `changed` flag into one span — mirrors `toSpans` in
/// the TS original.
fn push_span(spans: &mut Vec<WordSpan>, text: &str, changed: bool) {
    if let Some(last) = spans.last_mut()
        && last.changed == changed
    {
        last.text.push_str(text);
        return;
    }
    spans.push(WordSpan {
        text: text.to_owned(),
        changed,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_lines_get_no_marks() {
        assert_eq!(diff_words("let x = 1;", "let x = 1;"), None);
    }

    #[test]
    fn a_small_edit_marks_only_the_changed_word() {
        let diff = diff_words("let x = 1;", "let x = 2;").expect("should produce marks");
        let changed_del: String = diff
            .del
            .iter()
            .filter(|span| span.changed)
            .map(|span| span.text.as_str())
            .collect();
        let changed_add: String = diff
            .add
            .iter()
            .filter(|span| span.changed)
            .map(|span| span.text.as_str())
            .collect();
        assert!(changed_del.contains('1'), "del marks: {changed_del:?}");
        assert!(changed_add.contains('2'), "add marks: {changed_add:?}");
        // The unchanged prefix stays unmarked.
        assert!(
            diff.del
                .iter()
                .any(|span| !span.changed && span.text.contains("let x"))
        );
    }

    #[test]
    fn a_full_rewrite_gets_no_marks() {
        assert_eq!(
            diff_words(
                "const total = items.reduce((a, b) => a + b, 0)",
                "export default function App() {"
            ),
            None
        );
    }

    #[test]
    fn empty_sides_get_no_marks() {
        assert_eq!(diff_words("", "something"), None);
        assert_eq!(diff_words("something", ""), None);
    }
}
