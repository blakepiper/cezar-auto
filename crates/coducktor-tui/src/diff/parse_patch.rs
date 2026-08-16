//! Pure parsing + row building for the diff widget: one file's unified-diff section (the
//! server's `ChangedFile.patch` — `diff --git` headers plus `@@` hunks) → hunks with per-side
//! line numbers → renderable rows for the unified and split layouts, with word-level spans
//! attached to paired del/add lines and context gaps materialized for expansion.
//!
//! Ports `packages/web/src/components/diff/parse-patch.ts` 1:1 — same algorithm, same row
//! shapes. No terminal, no git library: this module only ever sees the patch text already on
//! `ChangedFile`.

use std::collections::HashMap;

use super::word_diff::{WordSpan, diff_words};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    pub kind: LineKind,
    /// Line text without its `+`/`-`/space marker.
    pub text: String,
    /// 1-based position in the old file — absent for adds.
    pub old_line: Option<u32>,
    /// 1-based position in the new file — absent for dels.
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The raw `@@ -a,b +c,d @@ …` header line.
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedPatch {
    pub hunks: Vec<Hunk>,
    /// The server capped the patch (`… (patch truncated)` marker) — tell the reader.
    pub truncated: bool,
}

const TRUNCATION_MARKER: &str = "… (patch truncated)";

/// Parses a `@@ -a,b +c,d @@ …` hunk header into `(old_start, old_count, new_start,
/// new_count)`, defaulting an omitted count to 1 (git's own convention for a one-line range).
/// A hand-rolled scan rather than a `regex` dependency — the grammar is small and fixed.
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_range, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let (new_range, rest) = rest.split_once(' ')?;
    if !rest.starts_with("@@") {
        return None;
    }
    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

pub fn parse_patch(patch: &str) -> ParsedPatch {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut truncated = false;
    let mut current: Option<usize> = None;
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    let mut raw: Vec<&str> = patch.split('\n').collect();
    // git ends every section with a newline; the split leaves a phantom '' that would
    // otherwise count as one extra empty context line.
    if raw.last() == Some(&"") {
        raw.pop();
    }

    for line in raw {
        if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(line) {
            hunks.push(Hunk {
                header: line.to_owned(),
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            old_line = old_start;
            new_line = new_start;
            current = Some(hunks.len() - 1);
            continue;
        }
        if line.contains(TRUNCATION_MARKER) {
            truncated = true;
            current = None; // the cap may have cut mid-hunk — stop attributing lines to it
            continue;
        }
        let Some(index) = current else {
            continue; // diff --git / index / ---/+++ headers, or non-diff preamble
        };
        let hunk = &mut hunks[index];
        if let Some(text) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine {
                kind: LineKind::Add,
                text: text.to_owned(),
                old_line: None,
                new_line: Some(new_line),
            });
            new_line += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine {
                kind: LineKind::Del,
                text: text.to_owned(),
                old_line: Some(old_line),
                new_line: None,
            });
            old_line += 1;
        } else if line.starts_with('\\') {
            // "\ No newline at end of file" — metadata, not content on either side.
        } else {
            // Context: ' ' + text, or a completely empty line (some tools emit '' for blank
            // context).
            let text = line.strip_prefix(' ').unwrap_or(line);
            hunk.lines.push(HunkLine {
                kind: LineKind::Context,
                text: text.to_owned(),
                old_line: Some(old_line),
                new_line: Some(new_line),
            });
            old_line += 1;
            new_line += 1;
        }
    }
    ParsedPatch { hunks, truncated }
}

// ---- context gaps -------------------------------------------------------------------------

/// An unchanged region the patch skipped: before the first hunk, between hunks, after the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextGap {
    /// Index of the hunk this gap precedes; `hunks.len()` for the trailing gap.
    pub before_hunk: usize,
    /// Hidden line count — `None` for the trailing gap (the patch doesn't know EOF).
    pub count: Option<u32>,
    /// First hidden line, old side.
    pub old_start: u32,
    /// First hidden line, new side.
    pub new_start: u32,
}

/// The expandable gaps of a hunk list. `include_trailing` — the after-last-hunk region has an
/// unknown length, so it is only worth a row when a loader can actually expand it.
pub fn context_gaps(hunks: &[Hunk], include_trailing: bool) -> Vec<ContextGap> {
    let mut gaps = Vec::new();
    let Some(first) = hunks.first() else {
        return gaps;
    };
    if first.new_start > 1 {
        gaps.push(ContextGap {
            before_hunk: 0,
            count: Some(first.new_start - 1),
            old_start: 1,
            new_start: 1,
        });
    }
    for i in 1..hunks.len() {
        let prev = &hunks[i - 1];
        let next = &hunks[i];
        let prev_old_end = prev.old_start + prev.old_count;
        let prev_new_end = prev.new_start + prev.new_count;
        let count = next.new_start as i64 - prev_new_end as i64;
        if count > 0 {
            gaps.push(ContextGap {
                before_hunk: i,
                count: Some(count as u32),
                old_start: prev_old_end,
                new_start: prev_new_end,
            });
        }
    }
    if include_trailing && let Some(last) = hunks.last() {
        gaps.push(ContextGap {
            before_hunk: hunks.len(),
            count: None,
            old_start: last.old_start + last.old_count,
            new_start: last.new_start + last.new_count,
        });
    }
    gaps
}

/// Materialize a gap's hidden lines as context rows from the file's current (new-side) text.
/// Old-side numbers follow at the gap's constant old/new offset — the region is unchanged, so
/// both sides advance in lockstep.
pub fn context_lines_for_gap(gap: &ContextGap, file_lines: &[&str]) -> Vec<HunkLine> {
    let last_new_line = gap
        .count
        .map(|count| gap.new_start + count - 1)
        .unwrap_or(file_lines.len() as u32);
    let offset = gap.old_start as i64 - gap.new_start as i64;
    let mut lines = Vec::new();
    let mut new_line = gap.new_start;
    while new_line <= last_new_line.min(file_lines.len() as u32) {
        let text = file_lines
            .get((new_line - 1) as usize)
            .copied()
            .unwrap_or("");
        lines.push(HunkLine {
            kind: LineKind::Context,
            text: text.to_owned(),
            old_line: Some((new_line as i64 + offset) as u32),
            new_line: Some(new_line),
        });
        new_line += 1;
    }
    lines
}

// ---- renderable rows ----------------------------------------------------------------------

/// A line plus its word-level marks (only paired del/add lines carry spans).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffCell {
    pub line: HunkLine,
    pub spans: Option<Vec<WordSpan>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedRow {
    Hunk(Hunk),
    Gap(ContextGap),
    Line(DiffCell),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitRow {
    Hunk(Hunk),
    Gap(ContextGap),
    Pair {
        left: Option<DiffCell>,
        right: Option<DiffCell>,
    },
}

/// Expanded gaps, keyed by `ContextGap::before_hunk` — the screen's expansion state.
pub type ExpandedGaps = HashMap<usize, Vec<HunkLine>>;

/// A change block: the consecutive del run and the add run that immediately follows it.
struct ChangeBlock {
    dels: Vec<HunkLine>,
    adds: Vec<HunkLine>,
    del_spans: Vec<Option<Vec<WordSpan>>>,
    add_spans: Vec<Option<Vec<WordSpan>>>,
}

/// Pair del\[i\] ↔ add\[i\] and compute word-level marks for each pair (null-safe past min
/// length).
fn pair_block(dels: Vec<HunkLine>, adds: Vec<HunkLine>) -> ChangeBlock {
    let pairs = dels.len().min(adds.len());
    let mut del_spans: Vec<Option<Vec<WordSpan>>> = vec![None; dels.len()];
    let mut add_spans: Vec<Option<Vec<WordSpan>>> = vec![None; adds.len()];
    for i in 0..pairs {
        if let Some(words) = diff_words(&dels[i].text, &adds[i].text) {
            del_spans[i] = Some(words.del);
            add_spans[i] = Some(words.add);
        }
    }
    ChangeBlock {
        dels,
        adds,
        del_spans,
        add_spans,
    }
}

/// One step of a hunk walk: an unchanged line, or a paired del/add change block.
enum WalkEvent<'a> {
    Context(&'a HunkLine),
    Block(ChangeBlock),
}

/// Walk a hunk's lines as context runs and change blocks, in order.
fn walk_hunk(hunk: &Hunk, mut on_event: impl FnMut(WalkEvent<'_>)) {
    let mut i = 0;
    while i < hunk.lines.len() {
        let line = &hunk.lines[i];
        if matches!(line.kind, LineKind::Del | LineKind::Add) {
            let mut dels = Vec::new();
            let mut adds = Vec::new();
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Del {
                dels.push(hunk.lines[i].clone());
                i += 1;
            }
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Add {
                adds.push(hunk.lines[i].clone());
                i += 1;
            }
            on_event(WalkEvent::Block(pair_block(dels, adds)));
        } else {
            on_event(WalkEvent::Context(line));
            i += 1;
        }
    }
}

fn gap_before(gaps: &[ContextGap], position: usize) -> Option<ContextGap> {
    gaps.iter().find(|gap| gap.before_hunk == position).copied()
}

pub fn build_unified_rows(
    hunks: &[Hunk],
    gaps: &[ContextGap],
    expanded: Option<&ExpandedGaps>,
) -> Vec<UnifiedRow> {
    let mut rows = Vec::new();
    let push_gap = |rows: &mut Vec<UnifiedRow>, position: usize| {
        let Some(gap) = gap_before(gaps, position) else {
            return;
        };
        if let Some(lines) = expanded.and_then(|expanded| expanded.get(&position)) {
            for line in lines {
                rows.push(UnifiedRow::Line(DiffCell {
                    line: line.clone(),
                    spans: None,
                }));
            }
        } else {
            rows.push(UnifiedRow::Gap(gap));
        }
    };
    for (index, hunk) in hunks.iter().enumerate() {
        push_gap(&mut rows, index);
        rows.push(UnifiedRow::Hunk(hunk.clone()));
        walk_hunk(hunk, |event| match event {
            WalkEvent::Context(line) => {
                rows.push(UnifiedRow::Line(DiffCell {
                    line: line.clone(),
                    spans: None,
                }));
            }
            WalkEvent::Block(block) => {
                for (i, line) in block.dels.into_iter().enumerate() {
                    rows.push(UnifiedRow::Line(DiffCell {
                        line,
                        spans: block.del_spans[i].clone(),
                    }));
                }
                for (i, line) in block.adds.into_iter().enumerate() {
                    rows.push(UnifiedRow::Line(DiffCell {
                        line,
                        spans: block.add_spans[i].clone(),
                    }));
                }
            }
        });
    }
    push_gap(&mut rows, hunks.len());
    rows
}

pub fn build_split_rows(
    hunks: &[Hunk],
    gaps: &[ContextGap],
    expanded: Option<&ExpandedGaps>,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let push_gap = |rows: &mut Vec<SplitRow>, position: usize| {
        let Some(gap) = gap_before(gaps, position) else {
            return;
        };
        if let Some(lines) = expanded.and_then(|expanded| expanded.get(&position)) {
            for line in lines {
                rows.push(SplitRow::Pair {
                    left: Some(DiffCell {
                        line: line.clone(),
                        spans: None,
                    }),
                    right: Some(DiffCell {
                        line: line.clone(),
                        spans: None,
                    }),
                });
            }
        } else {
            rows.push(SplitRow::Gap(gap));
        }
    };
    for (index, hunk) in hunks.iter().enumerate() {
        push_gap(&mut rows, index);
        rows.push(SplitRow::Hunk(hunk.clone()));
        walk_hunk(hunk, |event| match event {
            WalkEvent::Context(line) => {
                rows.push(SplitRow::Pair {
                    left: Some(DiffCell {
                        line: line.clone(),
                        spans: None,
                    }),
                    right: Some(DiffCell {
                        line: line.clone(),
                        spans: None,
                    }),
                });
            }
            WalkEvent::Block(block) => {
                let height = block.dels.len().max(block.adds.len());
                for i in 0..height {
                    let left = block.dels.get(i).cloned().map(|line| DiffCell {
                        line,
                        spans: block.del_spans[i].clone(),
                    });
                    let right = block.adds.get(i).cloned().map(|line| DiffCell {
                        line,
                        spans: block.add_spans[i].clone(),
                    });
                    rows.push(SplitRow::Pair { left, right });
                }
            }
        });
    }
    push_gap(&mut rows, hunks.len());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 111..222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,5 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"added\");\n }\n";

    #[test]
    fn parses_one_hunk_with_line_numbers() {
        let parsed = parse_patch(SAMPLE_PATCH);
        assert_eq!(parsed.hunks.len(), 1);
        assert!(!parsed.truncated);
        let hunk = &parsed.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.lines.len(), 5);
        assert_eq!(hunk.lines[0].kind, LineKind::Context);
        assert_eq!(hunk.lines[0].old_line, Some(1));
        assert_eq!(hunk.lines[0].new_line, Some(1));
        assert_eq!(hunk.lines[1].kind, LineKind::Del);
        assert_eq!(hunk.lines[1].old_line, Some(2));
        assert_eq!(hunk.lines[1].new_line, None);
        assert_eq!(hunk.lines[2].kind, LineKind::Add);
        assert_eq!(hunk.lines[2].new_line, Some(2));
    }

    #[test]
    fn truncation_marker_stops_attribution() {
        let patch = format!("{SAMPLE_PATCH}{TRUNCATION_MARKER}\n");
        let parsed = parse_patch(&patch);
        assert!(parsed.truncated);
        assert_eq!(parsed.hunks.len(), 1);
    }

    #[test]
    fn leading_gap_is_reported_when_the_hunk_does_not_start_at_line_one() {
        let patch = "@@ -10,2 +10,2 @@\n context\n-old\n+new\n";
        let parsed = parse_patch(patch);
        let gaps = context_gaps(&parsed.hunks, false);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].before_hunk, 0);
        assert_eq!(gaps[0].count, Some(9));
    }

    #[test]
    fn unified_rows_pair_dels_and_adds_in_order() {
        let parsed = parse_patch(SAMPLE_PATCH);
        let rows = build_unified_rows(&parsed.hunks, &[], None);
        // hunk header, context, del, add, add, context
        assert_eq!(rows.len(), 6);
        assert!(matches!(rows[0], UnifiedRow::Hunk(_)));
        assert!(matches!(&rows[2], UnifiedRow::Line(cell) if cell.line.kind == LineKind::Del));
        assert!(matches!(&rows[3], UnifiedRow::Line(cell) if cell.line.kind == LineKind::Add));
    }

    #[test]
    fn split_rows_pair_the_del_add_block_positionally() {
        let parsed = parse_patch(SAMPLE_PATCH);
        let rows = build_split_rows(&parsed.hunks, &[], None);
        let pairs: Vec<_> = rows
            .iter()
            .filter(|row| matches!(row, SplitRow::Pair { .. }))
            .collect();
        // context, (del|add), (none|add), context
        assert_eq!(pairs.len(), 4);
        let SplitRow::Pair { left, right } = pairs[1] else {
            unreachable!()
        };
        assert!(left.as_ref().unwrap().line.kind == LineKind::Del);
        assert!(right.as_ref().unwrap().line.kind == LineKind::Add);
        let SplitRow::Pair { left, right } = pairs[2] else {
            unreachable!()
        };
        assert!(left.is_none());
        assert!(right.as_ref().unwrap().line.kind == LineKind::Add);
    }

    /// Operationalizes A9's accept criterion — "a worktree diff renders identically in
    /// content to `GET /runs/:id/diff`": that route and `ChangedFile.patch` (what this parser
    /// consumes) are documented as the same unified-diff bytes (`server.ts`: "This file's
    /// unified-diff section"). Reconstructing the hunk section from the parsed structure and
    /// comparing it byte-for-byte against the original patch text is the strongest form of
    /// that check at the parsing layer that owns content fidelity.
    #[test]
    fn hunks_round_trip_to_the_original_patch_bytes() {
        let hunk_only = &SAMPLE_PATCH[SAMPLE_PATCH.find("@@").unwrap()..];
        let parsed = parse_patch(SAMPLE_PATCH);
        let mut reconstructed = String::new();
        for hunk in &parsed.hunks {
            reconstructed.push_str(&hunk.header);
            reconstructed.push('\n');
            for line in &hunk.lines {
                let marker = match line.kind {
                    LineKind::Context => ' ',
                    LineKind::Add => '+',
                    LineKind::Del => '-',
                };
                reconstructed.push(marker);
                reconstructed.push_str(&line.text);
                reconstructed.push('\n');
            }
        }
        assert_eq!(reconstructed, hunk_only);
    }

    #[test]
    fn expanded_gap_lines_replace_the_gap_row() {
        let patch = "@@ -3,1 +3,1 @@\n unchanged\n";
        let parsed = parse_patch(patch);
        let gaps = context_gaps(&parsed.hunks, false);
        let file_lines = ["one", "two", "unchanged"];
        let lines = context_lines_for_gap(&gaps[0], &file_lines);
        let mut expanded: ExpandedGaps = ExpandedGaps::new();
        expanded.insert(0, lines);
        let rows = build_unified_rows(&parsed.hunks, &gaps, Some(&expanded));
        assert!(matches!(rows[0], UnifiedRow::Line(_)));
        assert!(matches!(rows[1], UnifiedRow::Line(_)));
    }
}
