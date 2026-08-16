//! The diff widget itself: per-file fold state, expanded context gaps, unified/split layout,
//! word-level marks and syntax color composed into `ratatui::Line`s. Mirrors
//! `web/src/components/diff/diff-view.tsx`'s behaviors (module doc there): per-file collapse,
//! file-level stat, whitespace toggle (here: soft-wrap), and split-mode's ~140-column
//! degradation (spec §7.6).
//!
//! No virtualization: Phase A's accept criterion for this step is content-identical rendering
//! and the width-based mode degradation, not a scrolling-performance bound (that bound belongs
//! to the transcript, A7, whose content is unbounded in a way a single diff view — capped by
//! the server's own patch-truncation — is not). The whole diff renders into one `Vec<Line>`
//! and the screen scrolls it with a plain offset, the same approach `DiffFallback` takes on
//! the web side when its lazy engine chunk never loads.

use std::collections::{HashMap, HashSet};

use coducktor_contract::{ChangedFile, ChangedFileStatus};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{ColorCapability, Theme};

use super::highlight::{HighlightSpan, Highlighter};
use super::parse_patch::{
    ContextGap, DiffCell, ExpandedGaps, Hunk, HunkLine, LineKind, SplitRow, UnifiedRow,
    build_split_rows, build_unified_rows, context_gaps, context_lines_for_gap, parse_patch,
};
use super::word_diff::WordSpan;

/// Layout: one interleaved column, or old|new side by side. Default `Unified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Unified,
    Split,
}

impl DiffMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::Unified => Self::Split,
            Self::Split => Self::Unified,
        }
    }
}

/// Split mode degrades to unified below this width (spec §7.6: "degrades to unified below
/// ~140 columns").
pub const SPLIT_MIN_WIDTH: u16 = 140;

/// The mode actually used at a given viewport width — `Split` only ever renders at or above
/// `SPLIT_MIN_WIDTH`; narrower viewports get `Unified` regardless of what the user asked for.
/// This is the accept criterion's "split mode degrades below 140 columns", made a pure
/// function so it is unit-testable without a terminal.
pub fn effective_mode(requested: DiffMode, width: u16) -> DiffMode {
    if requested == DiffMode::Split && width < SPLIT_MIN_WIDTH {
        DiffMode::Unified
    } else {
        requested
    }
}

/// Stable identity for a file across refetches — mirrors `fileKey` in `diff-scroll.ts`.
pub fn file_key(file: &ChangedFile) -> String {
    format!("{}→{}", file.old_path.as_deref().unwrap_or(""), file.path)
}

/// Per-file interaction state a screen owns and mutates across renders — the Rust twin of
/// `DiffView`'s `collapsed`/`expandedByFile` React state, since virtualization is not in play
/// here there is no unmount risk, but the state still needs to survive a data refetch that
/// returns the same files (an active run polls its `/changes` route every few seconds).
#[derive(Debug, Clone, Default)]
pub struct DiffViewState {
    pub collapsed: HashSet<String>,
    pub expanded_by_file: HashMap<String, ExpandedGaps>,
    pub mode: DiffMode,
    pub wrap: bool,
}

impl DiffViewState {
    pub fn toggle_file(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
    }

    pub fn is_open(&self, key: &str) -> bool {
        !self.collapsed.contains(key)
    }

    /// Record a loaded gap's lines (the caller already fetched the file's current text through
    /// `Engine::run_files`/`Engine::repo_changes`'s sibling file-content route).
    pub fn expand_gap(&mut self, key: &str, before_hunk: usize, lines: Vec<HunkLine>) {
        self.expanded_by_file
            .entry(key.to_owned())
            .or_default()
            .insert(before_hunk, lines);
    }

    fn expanded_for(&self, key: &str) -> Option<&ExpandedGaps> {
        self.expanded_by_file.get(key)
    }
}

/// One clickable region a screen should register with the `HitMap`: fold a file, or expand a
/// gap. Returned alongside the rendered lines so the caller can map row index → action without
/// this module reaching into `crate::input::hitmap` itself (kept dependency-free of the app
/// shell, like `parse_patch`/`word_diff`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRowAction {
    ToggleFile { key: String },
    ExpandGap { key: String, before_hunk: usize },
}

/// Materialize a gap's hidden lines from the file's current text (fetched by the screen through
/// `Engine::run_files`/repo file content) — the piece `DiffViewState::expand_gap` needs.
pub fn materialize_gap(gap: &ContextGap, file_text: &str) -> Vec<HunkLine> {
    let lines: Vec<&str> = file_text.split('\n').collect();
    context_lines_for_gap(gap, &lines)
}

/// Render every file into one flat line buffer, `wrap`-aware, honoring fold/expand state and
/// the effective (width-degraded) mode. Returns the lines and a parallel action-per-row map
/// (only rows with an action are present) — the caller renders with a `Paragraph` and its own
/// scroll offset, and turns row index (`scroll_offset + area-relative y`) into an action.
pub fn render_files(
    files: &[ChangedFile],
    state: &DiffViewState,
    theme: &Theme,
    highlighter: &Highlighter,
    width: u16,
) -> (Vec<Line<'static>>, HashMap<usize, DiffRowAction>) {
    let mode = effective_mode(state.mode, width);
    let mut lines = Vec::new();
    let mut actions = HashMap::new();
    for file in files {
        render_file(
            file,
            state,
            theme,
            highlighter,
            mode,
            &mut lines,
            &mut actions,
        );
    }
    (lines, actions)
}

#[allow(clippy::too_many_arguments)]
fn render_file(
    file: &ChangedFile,
    state: &DiffViewState,
    theme: &Theme,
    highlighter: &Highlighter,
    mode: DiffMode,
    lines: &mut Vec<Line<'static>>,
    actions: &mut HashMap<usize, DiffRowAction>,
) {
    let key = file_key(file);
    let open = state.is_open(&key);
    actions.insert(lines.len(), DiffRowAction::ToggleFile { key: key.clone() });
    lines.push(file_header_line(file, open, theme));
    if !open {
        return;
    }

    if file.image.unwrap_or(false) {
        lines.push(note_line("Image file — preview in the Files tab.", theme));
        return;
    }
    if file.binary {
        lines.push(note_line("Binary file — no text diff.", theme));
        return;
    }

    let parsed = parse_patch(&file.patch);
    if parsed.hunks.is_empty() {
        let text = if parsed.truncated {
            "Patch truncated by the server."
        } else {
            "No content changes (metadata only)."
        };
        lines.push(note_line(text, theme));
        return;
    }

    let expandable = matches!(
        file.status,
        ChangedFileStatus::Modified | ChangedFileStatus::Renamed | ChangedFileStatus::Copied
    );
    let gaps = context_gaps(&parsed.hunks, expandable);
    let expanded = state.expanded_for(&key);

    let line_list = build_line_list(&parsed.hunks, expanded);
    let tokens = highlighter.highlight_lines(&file.path, &line_list.texts);

    match mode {
        DiffMode::Unified => {
            let rows = build_unified_rows(&parsed.hunks, &gaps, expanded);
            for row in rows {
                render_unified_row(
                    row,
                    &line_list,
                    tokens.as_deref(),
                    state.wrap,
                    theme,
                    &key,
                    lines,
                    actions,
                );
            }
        }
        DiffMode::Split => {
            let rows = build_split_rows(&parsed.hunks, &gaps, expanded);
            for row in rows {
                render_split_row(
                    row,
                    &line_list,
                    tokens.as_deref(),
                    state.wrap,
                    theme,
                    &key,
                    lines,
                    actions,
                );
            }
        }
    }
    if parsed.truncated {
        lines.push(note_line(
            "Patch truncated by the server — counts above remain exact.",
            theme,
        ));
    }
}

/// One ordered list of every displayed line (hunks + expanded context) — the highlighting
/// unit, mirroring `DiffFileBody`'s `lineList`/`lineIndex` in the TS original. `positions`
/// maps a `HunkLine` (by old/new-line identity) to its index in `texts` for `tokens_for`.
struct LineList {
    texts: Vec<String>,
    positions: HashMap<(Option<u32>, Option<u32>, LineKind), usize>,
}

fn build_line_list(hunks: &[Hunk], expanded: Option<&ExpandedGaps>) -> LineList {
    let mut texts = Vec::new();
    let mut positions = HashMap::new();
    let push = |line: &HunkLine, texts: &mut Vec<String>, positions: &mut HashMap<_, _>| {
        positions
            .entry((line.old_line, line.new_line, line.kind))
            .or_insert(texts.len());
        texts.push(line.text.clone());
    };
    for (index, hunk) in hunks.iter().enumerate() {
        if let Some(expansion) = expanded.and_then(|expanded| expanded.get(&index)) {
            for line in expansion {
                push(line, &mut texts, &mut positions);
            }
        }
        for line in &hunk.lines {
            push(line, &mut texts, &mut positions);
        }
    }
    if let Some(trailing) = expanded.and_then(|expanded| expanded.get(&hunks.len())) {
        for line in trailing {
            push(line, &mut texts, &mut positions);
        }
    }
    LineList { texts, positions }
}

fn tokens_for<'a>(
    list: &LineList,
    tokens: Option<&'a [Vec<HighlightSpan>]>,
    line: &HunkLine,
) -> Option<&'a [HighlightSpan]> {
    let tokens = tokens?;
    let index = *list
        .positions
        .get(&(line.old_line, line.new_line, line.kind))?;
    tokens.get(index).map(Vec::as_slice)
}

#[allow(clippy::too_many_arguments)]
fn render_unified_row(
    row: UnifiedRow,
    list: &LineList,
    tokens: Option<&[Vec<HighlightSpan>]>,
    wrap: bool,
    theme: &Theme,
    file_key: &str,
    lines: &mut Vec<Line<'static>>,
    actions: &mut HashMap<usize, DiffRowAction>,
) {
    match row {
        UnifiedRow::Hunk(hunk) => lines.push(hunk_header_line(&hunk, theme)),
        UnifiedRow::Gap(gap) => {
            actions.insert(
                lines.len(),
                DiffRowAction::ExpandGap {
                    key: file_key.to_owned(),
                    before_hunk: gap.before_hunk,
                },
            );
            lines.push(gap_line(&gap, theme));
        }
        UnifiedRow::Line(cell) => {
            let line_tokens = tokens_for(list, tokens, &cell.line);
            lines.push(unified_line(&cell, line_tokens, wrap, theme));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_split_row(
    row: SplitRow,
    list: &LineList,
    tokens: Option<&[Vec<HighlightSpan>]>,
    wrap: bool,
    theme: &Theme,
    file_key: &str,
    lines: &mut Vec<Line<'static>>,
    actions: &mut HashMap<usize, DiffRowAction>,
) {
    match row {
        SplitRow::Hunk(hunk) => lines.push(hunk_header_line(&hunk, theme)),
        SplitRow::Gap(gap) => {
            actions.insert(
                lines.len(),
                DiffRowAction::ExpandGap {
                    key: file_key.to_owned(),
                    before_hunk: gap.before_hunk,
                },
            );
            lines.push(gap_line(&gap, theme));
        }
        SplitRow::Pair { left, right } => {
            let left_tokens = left
                .as_ref()
                .and_then(|cell| tokens_for(list, tokens, &cell.line));
            let right_tokens = right
                .as_ref()
                .and_then(|cell| tokens_for(list, tokens, &cell.line));
            lines.push(split_pair_line(
                left.as_ref(),
                right.as_ref(),
                left_tokens,
                right_tokens,
                wrap,
                theme,
            ));
        }
    }
}

fn file_header_line(file: &ChangedFile, open: bool, theme: &Theme) -> Line<'static> {
    let chevron = if open { "▾" } else { "▸" };
    let path = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let badge = match file.status {
        ChangedFileStatus::Added => Some("added"),
        ChangedFileStatus::Deleted => Some("deleted"),
        ChangedFileStatus::Renamed => Some("renamed"),
        ChangedFileStatus::Copied => Some("copied"),
        ChangedFileStatus::Modified => None,
    };
    let mut spans = vec![
        Span::styled(
            format!("{chevron} "),
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled(
            path,
            Style::default()
                .fg(theme.palette.fg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(badge) = badge {
        spans.push(Span::styled(
            format!("  [{badge}]"),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    spans.push(Span::styled(
        format!("  +{} -{}", file.adds as i64, file.dels as i64),
        Style::default().fg(theme.palette.soft_fg),
    ));
    Line::from(spans)
}

fn note_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default().fg(theme.palette.soft_fg),
    ))
}

fn hunk_header_line(hunk: &Hunk, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        hunk.header.clone(),
        Style::default().fg(theme.palette.soft_fg),
    ))
}

fn gap_line(gap: &ContextGap, theme: &Theme) -> Line<'static> {
    let label = match gap.count {
        None => "⋯ unchanged lines to end of file — expand".to_owned(),
        Some(1) => "⋯ 1 unchanged line — expand".to_owned(),
        Some(count) => format!("⋯ {count} unchanged lines — expand"),
    };
    Line::from(Span::styled(
        label,
        Style::default().fg(theme.palette.soft_fg),
    ))
}

fn marker(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Add => "+",
        LineKind::Del => "−",
        LineKind::Context => " ",
    }
}

fn gutter(value: Option<u32>) -> String {
    match value {
        Some(value) => format!("{value:>5} "),
        None => "      ".to_owned(),
    }
}

fn content_spans(
    cell: &DiffCell,
    tokens: Option<&[HighlightSpan]>,
    theme: &Theme,
) -> Vec<Span<'static>> {
    overlay_segments(tokens, cell.spans.as_deref(), &cell.line.text)
        .into_iter()
        .map(|segment| {
            let mut style = Style::default();
            if let Some(color) = segment.color {
                style = style.fg(color);
            }
            if segment.changed {
                style = style
                    .bg(match cell.line.kind {
                        LineKind::Add => theme.palette.add,
                        _ => theme.palette.del,
                    })
                    .add_modifier(Modifier::BOLD);
            }
            Span::styled(segment.text, style)
        })
        .collect()
}

fn unified_line(
    cell: &DiffCell,
    tokens: Option<&[HighlightSpan]>,
    wrap: bool,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        gutter(cell.line.old_line),
        Style::default().fg(theme.palette.soft_fg),
    )];
    spans.push(Span::styled(
        gutter(cell.line.new_line),
        Style::default().fg(theme.palette.soft_fg),
    ));
    spans.push(Span::styled(
        format!("{} ", marker(cell.line.kind)),
        Style::default().fg(theme.palette.soft_fg),
    ));
    spans.extend(content_spans(cell, tokens, theme));
    let _ = wrap; // wrapping is a `Paragraph`-level concern the caller applies uniformly.
    Line::from(tint_row(spans, row_tint(cell.line.kind, theme)))
}

fn split_pair_line(
    left: Option<&DiffCell>,
    right: Option<&DiffCell>,
    left_tokens: Option<&[HighlightSpan]>,
    right_tokens: Option<&[HighlightSpan]>,
    wrap: bool,
    theme: &Theme,
) -> Line<'static> {
    let _ = wrap;
    let mut left_spans = Vec::new();
    match left {
        Some(cell) => {
            left_spans.push(Span::styled(
                gutter(cell.line.old_line),
                Style::default().fg(theme.palette.soft_fg),
            ));
            left_spans.push(Span::styled(
                format!("{} ", marker(cell.line.kind)),
                Style::default().fg(theme.palette.soft_fg),
            ));
            left_spans.extend(content_spans(cell, left_tokens, theme));
            left_spans = tint_row(left_spans, row_tint(cell.line.kind, theme));
        }
        None => left_spans.push(Span::styled(
            "      ",
            Style::default().fg(theme.palette.soft_fg),
        )),
    }
    let mut spans = left_spans;
    spans.push(Span::styled(
        " │ ",
        Style::default().fg(theme.palette.border),
    ));
    if let Some(cell) = right {
        let mut right_spans = vec![
            Span::styled(
                gutter(cell.line.new_line),
                Style::default().fg(theme.palette.soft_fg),
            ),
            Span::styled(
                format!("{} ", marker(cell.line.kind)),
                Style::default().fg(theme.palette.soft_fg),
            ),
        ];
        right_spans.extend(content_spans(cell, right_tokens, theme));
        spans.extend(tint_row(right_spans, row_tint(cell.line.kind, theme)));
    }
    Line::from(spans)
}

/// A soft background tint for whole add/del rows (spec §7.6: "overlay add/del background
/// tint"), blended toward the theme's surface color. Only applied at true-color capability —
/// on 256/16-color terminals the accent colors are too saturated to use as a full-row wash, so
/// the marker plus the word-level marks (`content_spans`) carry the signal there instead.
fn row_tint(kind: LineKind, theme: &Theme) -> Option<Color> {
    if theme.capability != ColorCapability::TrueColor {
        return None;
    }
    let accent = match kind {
        LineKind::Add => theme.palette.add,
        LineKind::Del => theme.palette.del,
        LineKind::Context => return None,
    };
    blend(accent, theme.palette.bg, 0.16)
}

fn blend(accent: Color, base: Color, amount: f32) -> Option<Color> {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (accent, base) else {
        return Some(accent);
    };
    let mix = |a: u8, b: u8| ((a as f32 * amount) + (b as f32 * (1.0 - amount))).round() as u8;
    Some(Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb)))
}

/// Apply a row-level background to every span that doesn't already carry one (a word-marked
/// span's stronger background wins over the row wash).
fn tint_row(spans: Vec<Span<'static>>, color: Option<Color>) -> Vec<Span<'static>> {
    let Some(color) = color else {
        return spans;
    };
    spans
        .into_iter()
        .map(|span| {
            if span.style.bg.is_some() {
                span
            } else {
                Span::styled(span.content, span.style.bg(color))
            }
        })
        .collect()
}

/// One renderable run of a diff line: text with an optional syntax color and a word mark.
/// Ports `overlaySegments` from `word-diff.ts` — merges a line's syntax tokens with its word
/// spans by splitting at every boundary of either sequence, so highlighting and word emphasis
/// compose instead of competing.
struct RenderSegment {
    text: String,
    color: Option<Color>,
    changed: bool,
}

fn overlay_segments(
    tokens: Option<&[HighlightSpan]>,
    spans: Option<&[WordSpan]>,
    text: &str,
) -> Vec<RenderSegment> {
    let spans = match spans {
        Some(spans) if !spans.is_empty() => spans,
        _ => {
            return match tokens {
                None => {
                    if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![RenderSegment {
                            text: text.to_owned(),
                            color: None,
                            changed: false,
                        }]
                    }
                }
                Some(tokens) => tokens
                    .iter()
                    .map(|token| RenderSegment {
                        text: token.text.clone(),
                        color: Some(token.color),
                        changed: false,
                    })
                    .collect(),
            };
        }
    };
    let Some(tokens) = tokens.filter(|tokens| !tokens.is_empty()) else {
        return spans
            .iter()
            .map(|span| RenderSegment {
                text: span.text.clone(),
                color: None,
                changed: span.changed,
            })
            .collect();
    };

    let mut out = Vec::new();
    let (mut ti, mut t_off) = (0usize, 0usize);
    let (mut si, mut s_off) = (0usize, 0usize);
    while ti < tokens.len() && si < spans.len() {
        let token = &tokens[ti];
        let span = &spans[si];
        let t_remaining = token.text.len() - t_off;
        let s_remaining = span.text.len() - s_off;
        let length = t_remaining.min(s_remaining);
        if length > 0 {
            out.push(RenderSegment {
                text: token.text[t_off..t_off + length].to_owned(),
                color: Some(token.color),
                changed: span.changed,
            });
            t_off += length;
            s_off += length;
        }
        if token.text.len().saturating_sub(t_off) == 0 {
            ti += 1;
            t_off = 0;
        }
        if span.text.len().saturating_sub(s_off) == 0 {
            si += 1;
            s_off = 0;
        }
    }
    // Length disagreements (a truncated token stream) degrade to unmarked tails, never a loss.
    while ti < tokens.len() {
        let token = &tokens[ti];
        let tail = &token.text[t_off..];
        if !tail.is_empty() {
            out.push(RenderSegment {
                text: tail.to_owned(),
                color: Some(token.color),
                changed: false,
            });
        }
        ti += 1;
        t_off = 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file() -> ChangedFile {
        ChangedFile {
            path: "src/lib.rs".to_owned(),
            old_path: None,
            status: ChangedFileStatus::Modified,
            adds: 2.0,
            dels: 1.0,
            binary: false,
            image: None,
            patch: "@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"added\");\n }\n".to_owned(),
        }
    }

    #[test]
    fn effective_mode_degrades_split_below_140_columns() {
        assert_eq!(effective_mode(DiffMode::Split, 139), DiffMode::Unified);
        assert_eq!(effective_mode(DiffMode::Split, 140), DiffMode::Split);
        assert_eq!(effective_mode(DiffMode::Unified, 60), DiffMode::Unified);
    }

    #[test]
    fn rendering_a_file_reproduces_every_hunk_line_of_content() {
        let file = sample_file();
        let theme = Theme::new(
            crate::theme::ThemeName::Dark,
            crate::theme::ColorCapability::TrueColor,
        );
        let highlighter = Highlighter::new(true);
        let state = DiffViewState::default();
        let (lines, _) = render_files(
            std::slice::from_ref(&file),
            &state,
            &theme,
            &highlighter,
            200,
        );
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "println!(\"old\");",
            "println!(\"new\");",
            "println!(\"added\");",
            "fn main() {",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_collapsed_file_renders_only_its_header() {
        let file = sample_file();
        let theme = Theme::new(
            crate::theme::ThemeName::Dark,
            crate::theme::ColorCapability::TrueColor,
        );
        let highlighter = Highlighter::new(true);
        let mut state = DiffViewState::default();
        state.toggle_file(&file_key(&file));
        let (lines, _) = render_files(
            std::slice::from_ref(&file),
            &state,
            &theme,
            &highlighter,
            200,
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn a_binary_file_shows_a_note_instead_of_rows() {
        let mut file = sample_file();
        file.binary = true;
        file.patch = String::new();
        let theme = Theme::new(
            crate::theme::ThemeName::Dark,
            crate::theme::ColorCapability::TrueColor,
        );
        let highlighter = Highlighter::new(true);
        let state = DiffViewState::default();
        let (lines, _) = render_files(
            std::slice::from_ref(&file),
            &state,
            &theme,
            &highlighter,
            200,
        );
        assert_eq!(lines.len(), 2);
    }
}
