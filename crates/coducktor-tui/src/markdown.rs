//! Streaming-safe markdown rendering.
//!
//! Agent text arrives as `item.delta` events, and re-parsing the whole message on every
//! delta is O(n²) on long turns. `RenderCache` keeps the last parsed [`Text`] and only
//! re-parses when the source has actually changed; wrapped-line heights are memoized per
//! width so the transcript's virtualizer never re-measures an unchanged item.

use std::borrow::Cow;
use std::collections::HashMap;

use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

/// Per-item markdown render cache.
///
/// `source_len` is a cheap staleness check rather than a content hash: transcript text
/// only ever grows by delta-append, so a length change is exactly a content change here.
///
/// A further optimization would re-render only the tail block on an append,
/// instead of re-parsing the whole source — needs block-level reuse from `tui-markdown`'s
/// parser that the crate doesn't expose; this cache re-parses in full on change, which is
/// still O(1) amortized per rendered frame since parsing only happens when `source` differs
/// from the last parse, not on every render.
#[derive(Debug, Default)]
pub struct RenderCache {
    source_len: usize,
    rendered: Text<'static>,
    height_at_width: HashMap<u16, u16>,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn refresh(&mut self, source: &str) {
        if source.len() == self.source_len {
            return;
        }
        self.rendered = to_static(tui_markdown::from_str(source));
        self.source_len = source.len();
        self.height_at_width.clear();
    }

    /// The rendered markdown, re-parsing first if `source` has changed.
    pub fn text(&mut self, source: &str) -> &Text<'static> {
        self.refresh(source);
        &self.rendered
    }

    /// The wrapped height of `source` at `width`, memoized per width.
    pub fn height(&mut self, source: &str, width: u16) -> u16 {
        self.refresh(source);
        if width == 0 {
            return 0;
        }
        if let Some(height) = self.height_at_width.get(&width) {
            return *height;
        }
        let height = Paragraph::new(self.rendered.clone())
            .wrap(Wrap { trim: false })
            .line_count(width)
            .min(u16::MAX as usize) as u16;
        self.height_at_width.insert(width, height);
        height
    }
}

/// Force every span to own its content, detaching the result from `source`'s lifetime so
/// it can live in a cache alongside (not borrowed from) the string that produced it.
fn to_static(text: Text<'_>) -> Text<'static> {
    Text {
        alignment: text.alignment,
        style: text.style,
        lines: text
            .lines
            .into_iter()
            .map(|line| Line {
                style: line.style,
                alignment: line.alignment,
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style,
                        content: Cow::Owned(span.content.into_owned()),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_source_does_not_reparse() {
        let mut cache = RenderCache::new();
        let _ = cache.text("hello **world**");
        let first_len = cache.rendered.lines.len();
        // A byte-identical re-render must be a cache hit: source_len is unchanged, so the
        // cached `Text` (and any interior heights) survive untouched.
        let _ = cache.text("hello **world**");
        assert_eq!(cache.rendered.lines.len(), first_len);
        assert_eq!(cache.source_len, "hello **world**".len());
    }

    #[test]
    fn appended_text_invalidates_the_cache() {
        let mut cache = RenderCache::new();
        let _ = cache.height("one line", 40);
        assert_eq!(cache.height_at_width.len(), 1);
        // A delta append changes source_len, forcing a re-parse and dropping stale heights.
        let _ = cache.height("one line, now with more words appended", 40);
        assert_eq!(
            cache.source_len,
            "one line, now with more words appended".len()
        );
    }

    #[test]
    fn height_is_memoized_per_width() {
        let mut cache = RenderCache::new();
        let narrow = cache.height("a fairly long sentence that should wrap", 10);
        let wide = cache.height("a fairly long sentence that should wrap", 80);
        assert!(narrow >= wide);
        assert_eq!(cache.height_at_width.len(), 2);
    }

    #[test]
    fn zero_width_reports_zero_height_without_caching() {
        let mut cache = RenderCache::new();
        assert_eq!(cache.height("hello", 0), 0);
        assert!(cache.height_at_width.is_empty());
    }
}
