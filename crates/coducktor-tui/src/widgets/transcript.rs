//! The virtualized transcript item list (spec §7.6 "Transcript virtualization", §8.4
//! `transcript.rs`).
//!
//! Threads reach thousands of items, so this widget never lays out more than the
//! current viewport: a `(item revision, width)`-keyed height cache lets it skip
//! straight to the visible window, and each visible item is painted into a scratch
//! buffer once and blitted into place — the same technique gives pixel/row-accurate
//! scrolling even for an item that straddles the viewport's top or bottom edge.
//!
//! What this module does NOT do (left to spec §8.4's other sub-modules, built at A8):
//! ask cards, review panels, provider-auth-required cards, and the v1/v2 thread
//! reducer that turns a run's raw event log into the items rendered here.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use serde_json::Value;

use coducktor_protocol::{MessageRole, ToolKind, ToolStatus, tool_display};

use crate::image::{self, DecodedImage, ImageSupport};
use crate::markdown::RenderCache;
use crate::theme::Theme;

/// Finished tool output beyond this many lines clamps with a "+N more" note (mirrors
/// `OUTPUT_CLAMP_LINES` in `thread-items.tsx`; the web's separate "Show all" toggle is
/// left for A8, which owns interactive interior state for the thread screen).
const OUTPUT_CLAMP_LINES: u16 = 12;
/// Every image item reserves at most this many rows, regardless of its own aspect
/// ratio, so transcript height math never depends on what the image protocol decides
/// to do at render time.
const IMAGE_MAX_ROWS: u16 = 16;

/// One transcript entry. Deliberately a small, closed set: this is the rendering
/// primitive, not the thread reducer's full `ThreadEntry` union (no ask/provider-auth
/// cards — those are interactive, run-aware, and belong to A8).
pub enum TranscriptItem {
    Message(MessageItem),
    Reasoning(ReasoningItem),
    Tool(ToolItem),
    Note(NoteItem),
    Image(ImageItem),
}

impl TranscriptItem {
    pub fn id(&self) -> &str {
        match self {
            Self::Message(item) => &item.id,
            Self::Reasoning(item) => &item.id,
            Self::Tool(item) => &item.id,
            Self::Note(item) => &item.id,
            Self::Image(item) => &item.id,
        }
    }

    /// A cheap fingerprint of everything that can change an item's rendered height.
    /// The transcript's height cache recomputes only when this changes.
    fn revision(&self) -> u64 {
        match self {
            Self::Message(item) => item.text.len() as u64,
            Self::Reasoning(item) => (item.text.len() as u64) << 1 | u64::from(item.expanded),
            Self::Tool(item) => {
                let mut bits = item.status as u64;
                bits = bits
                    .wrapping_mul(31)
                    .wrapping_add(item.output.as_ref().map_or(0, String::len) as u64);
                bits = bits
                    .wrapping_mul(31)
                    .wrapping_add(item.error.as_ref().map_or(0, String::len) as u64);
                bits = bits
                    .wrapping_mul(31)
                    .wrapping_add(match item.user_expanded {
                        None => 2,
                        Some(false) => 0,
                        Some(true) => 1,
                    });
                bits
            }
            Self::Note(item) => item.text.len() as u64,
            // Immutable once decoded: there is nothing that can change its height.
            Self::Image(_) => 0,
        }
    }

    fn height(&mut self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }
        match self {
            Self::Message(item) => {
                let inner =
                    width.saturating_sub(if item.role == MessageRole::User { 1 } else { 0 });
                item.cache.height(&item.text, inner)
            }
            Self::Reasoning(item) => {
                // A mapper regression that mints an empty reasoning item degrades quietly
                // rather than rendering a bare, un-expandable row (mirrors thread-items.tsx).
                if item.text.trim().is_empty() {
                    return 0;
                }
                if item.expanded {
                    1 + item.cache.height(&item.text, width.saturating_sub(2))
                } else {
                    1
                }
            }
            Self::Tool(item) => tool_card_height(item, width),
            Self::Note(item) => wrapped_line_count(&item.text, width.saturating_sub(2)).max(1),
            Self::Image(item) => image_height(item, width),
        }
    }

    fn paint(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        match self {
            Self::Message(item) => paint_message(item, buf, area, theme),
            Self::Reasoning(item) => paint_reasoning(item, buf, area, theme),
            Self::Tool(item) => paint_tool_card(item, buf, area, theme),
            Self::Note(item) => paint_note(item, buf, area, theme),
            Self::Image(item) => paint_image(item, buf, area, theme),
        }
    }
}

pub struct MessageItem {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    cache: RenderCache,
}

impl MessageItem {
    pub fn new(id: impl Into<String>, role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            text: text.into(),
            cache: RenderCache::new(),
        }
    }
}

pub struct ReasoningItem {
    pub id: String,
    pub text: String,
    pub expanded: bool,
    cache: RenderCache,
}

impl ReasoningItem {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            expanded: false,
            cache: RenderCache::new(),
        }
    }
}

/// A tool invocation, display fields precomputed once via [`tool_display`] (spec
/// §7.6's "tool cards from `tool-display`") rather than every render.
pub struct ToolItem {
    pub id: String,
    pub tool_kind: ToolKind,
    pub title: String,
    pub subtitle: Option<String>,
    pub status: ToolStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub exit_code: Option<i64>,
    /// `None` follows the default-open policy (`tool_default_open`); `Some` is the
    /// user's explicit toggle, which always wins once they touch the card.
    pub user_expanded: Option<bool>,
}

impl ToolItem {
    pub fn new(
        id: impl Into<String>,
        name: &str,
        input: Option<&Value>,
        status: ToolStatus,
    ) -> Self {
        let display = tool_display(name, input);
        Self {
            id: id.into(),
            tool_kind: display.tool_kind,
            title: display.title,
            subtitle: display.subtitle,
            status,
            output: None,
            error: None,
            exit_code: None,
            user_expanded: None,
        }
    }

    fn has_detail(&self) -> bool {
        self.output.as_deref().is_some_and(|text| !text.is_empty())
            || self.error.as_deref().is_some_and(|text| !text.is_empty())
    }

    fn open(&self) -> bool {
        self.has_detail()
            && self
                .user_expanded
                .unwrap_or_else(|| tool_default_open(self))
    }
}

/// Mirrors `defaultOpen` in `thread-items.tsx`: a running command with output already
/// streaming in opens to show its live tail; everything else — edits, finished
/// commands, and failures — starts closed but visible.
fn tool_default_open(item: &ToolItem) -> bool {
    item.tool_kind == ToolKind::Execute
        && item.status == ToolStatus::Running
        && item.output.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteTone {
    Dim,
    Warning,
    Danger,
}

pub struct NoteItem {
    pub id: String,
    pub text: String,
    pub tone: NoteTone,
}

impl NoteItem {
    pub fn new(id: impl Into<String>, text: impl Into<String>, tone: NoteTone) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            tone,
        }
    }
}

pub struct ImageItem {
    pub id: String,
    decoded: Option<DecodedImage>,
    dimensions: Option<(u32, u32)>,
    reason: &'static str,
}

impl ImageItem {
    /// Decodes once, at construction — the item is immutable after that, so nothing
    /// downstream needs to re-decode on every frame or re-check protocol support.
    pub fn decode(id: impl Into<String>, support: &ImageSupport, data_base64: &str) -> Self {
        match support.decode(data_base64) {
            Ok(decoded) => Self {
                id: id.into(),
                dimensions: Some(decoded.dimensions),
                decoded: Some(decoded),
                reason: "",
            },
            Err(error) => Self {
                id: id.into(),
                dimensions: None,
                decoded: None,
                reason: error.reason(),
            },
        }
    }
}

fn tool_card_height(item: &ToolItem, width: u16) -> u16 {
    let mut height = 1; // the collapsed header row is always shown
    if item.open() {
        if let Some(error) = item.error.as_deref().filter(|text| !text.is_empty()) {
            height += wrapped_line_count(error, width.saturating_sub(2)).max(1);
        }
        if let Some(output) = item.output.as_deref().filter(|text| !text.is_empty()) {
            let lines = output.lines().count().max(1) as u16;
            let shown = lines.min(OUTPUT_CLAMP_LINES);
            height += shown;
            if lines > OUTPUT_CLAMP_LINES {
                height += 1; // the "+N more lines" note
            }
        }
    }
    height
}

fn image_height(item: &ImageItem, width: u16) -> u16 {
    match item.dimensions {
        Some((pixel_width, pixel_height)) if item.decoded.is_some() => {
            // Terminal cells are roughly twice as tall as they are wide, so a `cols x
            // rows` box covers about `cols x (rows*2)` pixels. This is a display-height
            // estimate for virtualization only — `StatefulImage`'s own `Resize::Fit`
            // still decides the actual encoded size at render time, bounded to fit.
            let cols = width.clamp(1, 60) as f64;
            let rows = (pixel_height as f64 / pixel_width.max(1) as f64 * cols / 2.0).ceil();
            (rows as u16).clamp(3, IMAGE_MAX_ROWS)
        }
        _ => 4, // bordered placeholder: reason line + open-externally line + border
    }
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .min(u16::MAX as usize) as u16
}

fn paint_message(item: &mut MessageItem, buf: &mut Buffer, area: Rect, theme: &Theme) {
    let style = Style::default().fg(theme.palette.fg);
    if item.role == MessageRole::User {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme.palette.border));
        let inner = block.inner(area);
        block.render(area, buf);
        Paragraph::new(item.cache.text(&item.text).clone())
            .style(style)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    } else {
        Paragraph::new(item.cache.text(&item.text).clone())
            .style(style)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

fn paint_reasoning(item: &mut ReasoningItem, buf: &mut Buffer, area: Rect, theme: &Theme) {
    if item.text.trim().is_empty() {
        return;
    }
    let soft = Style::default().fg(theme.palette.soft_fg);
    let chevron = if item.expanded {
        '\u{25be}'
    } else {
        '\u{25b8}'
    };
    let first_line = item.text.lines().next().unwrap_or_default();
    let header = Rect::new(area.x, area.y, area.width, 1.min(area.height));
    Line::from(vec![
        Span::styled(format!("{chevron} Thinking \u{2014} "), soft),
        Span::styled(first_line.to_owned(), soft.add_modifier(Modifier::DIM)),
    ])
    .render(header, buf);
    if item.expanded && area.height > 1 {
        let body = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height - 1,
        );
        Paragraph::new(item.cache.text(&item.text).clone())
            .style(soft)
            .wrap(Wrap { trim: false })
            .render(body, buf);
    }
}

fn paint_note(item: &NoteItem, buf: &mut Buffer, area: Rect, theme: &Theme) {
    let (prefix, color) = match item.tone {
        NoteTone::Danger => ("\u{2717} ", theme.palette.failed),
        NoteTone::Warning => ("\u{21c4} ", theme.palette.waiting),
        NoteTone::Dim => ("\u{b7} ", theme.palette.soft_fg),
    };
    Paragraph::new(format!("{prefix}{}", item.text))
        .style(Style::default().fg(color))
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

fn paint_tool_card(item: &ToolItem, buf: &mut Buffer, area: Rect, theme: &Theme) {
    let open = item.open();
    let chevron = if !item.has_detail() {
        ' '
    } else if open {
        '\u{25be}'
    } else {
        '\u{25b8}'
    };
    let title_style = Style::default()
        .fg(theme.palette.fg)
        .add_modifier(Modifier::BOLD);
    let mut header = vec![
        Span::styled(
            format!("{chevron} "),
            Style::default().fg(theme.palette.soft_fg),
        ),
        Span::styled(item.title.clone(), title_style),
    ];
    if let Some(subtitle) = item.subtitle.as_deref().filter(|text| !text.is_empty()) {
        header.push(Span::raw(" "));
        header.push(Span::styled(
            subtitle.to_owned(),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    match item.status {
        ToolStatus::Running | ToolStatus::Pending => {
            header.push(Span::styled(
                " running",
                Style::default().fg(theme.palette.running),
            ));
        }
        ToolStatus::Failed => {
            header.push(Span::styled(
                " failed",
                Style::default().fg(theme.palette.failed),
            ));
        }
        ToolStatus::Declined => {
            header.push(Span::styled(
                " declined",
                Style::default().fg(theme.palette.soft_fg),
            ));
        }
        ToolStatus::Completed => {}
    }
    if item.tool_kind == ToolKind::Execute
        && let Some(exit_code) = item.exit_code
    {
        let color = if exit_code == 0 {
            theme.palette.done
        } else {
            theme.palette.failed
        };
        header.push(Span::styled(
            format!(" [{exit_code}]"),
            Style::default().fg(color),
        ));
    }
    let header_area = Rect::new(area.x, area.y, area.width, 1.min(area.height));
    Line::from(header).render(header_area, buf);

    if !open || area.height <= 1 {
        return;
    }
    let mut y = area.y + 1;
    let bottom = area.y + area.height;
    let body_width = area.width.saturating_sub(2);
    if let Some(error) = item.error.as_deref().filter(|text| !text.is_empty())
        && y < bottom
    {
        let error_lines = wrapped_line_count(error, body_width).max(1).min(bottom - y);
        let rect = Rect::new(area.x + 2, y, body_width, error_lines);
        Paragraph::new(error.to_owned())
            .style(Style::default().fg(theme.palette.failed))
            .wrap(Wrap { trim: false })
            .render(rect, buf);
        y += error_lines;
    }
    if let Some(output) = item.output.as_deref().filter(|text| !text.is_empty())
        && y < bottom
    {
        let total_lines = output.lines().count().max(1) as u16;
        let shown = total_lines.min(OUTPUT_CLAMP_LINES).min(bottom - y);
        let rect = Rect::new(area.x + 2, y, body_width, shown);
        let clamped: String = output
            .lines()
            .take(shown as usize)
            .collect::<Vec<_>>()
            .join("\n");
        Paragraph::new(clamped)
            .style(Style::default().fg(theme.palette.soft_fg))
            .render(rect, buf);
        y += shown;
        if total_lines > OUTPUT_CLAMP_LINES && y < bottom {
            let more = total_lines - OUTPUT_CLAMP_LINES;
            Line::from(Span::styled(
                format!("+{more} more lines"),
                Style::default()
                    .fg(theme.palette.soft_fg)
                    .add_modifier(Modifier::DIM),
            ))
            .render(Rect::new(area.x + 2, y, body_width, 1), buf);
        }
    }
}

fn paint_image(item: &mut ImageItem, buf: &mut Buffer, area: Rect, theme: &Theme) {
    let style = Style::default().fg(theme.palette.border);
    if item.decoded.is_some() {
        image::render_image(area, buf, style, &mut item.decoded);
    } else {
        image::render_placeholder(area, buf, style, item.dimensions, item.reason);
    }
}

/// The scrollable, virtualized item list. Owns its own scroll position; the host
/// screen owns focus, selection and which items exist.
pub struct Transcript {
    items: Vec<TranscriptItem>,
    scroll_offset: u32,
    /// Stays pinned to the bottom while the viewer hasn't scrolled up — mirrors
    /// `thread-scroller.tsx`'s stick-to-bottom / preserve-anchor split.
    sticky_bottom: bool,
    height_cache: HeightCache,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            scroll_offset: 0,
            sticky_bottom: true,
            height_cache: HeightCache::default(),
        }
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    pub fn push(&mut self, item: TranscriptItem) {
        self.items.push(item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Toggle a reasoning item's expanded state (bound to `Tab` by the host screen).
    pub fn toggle_reasoning(&mut self, id: &str) {
        if let Some(TranscriptItem::Reasoning(item)) =
            self.items.iter_mut().find(|item| item.id() == id)
        {
            item.expanded = !item.expanded;
        }
    }

    /// Toggle a tool card's user-controlled open/closed state.
    pub fn toggle_tool(&mut self, id: &str) {
        if let Some(TranscriptItem::Tool(item)) = self.items.iter_mut().find(|item| item.id() == id)
        {
            item.user_expanded = Some(!item.open());
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.sticky_bottom = false;
        self.scroll_offset = self.scroll_offset.saturating_add_signed(delta);
    }

    pub fn jump_to_bottom(&mut self) {
        self.sticky_bottom = true;
    }

    fn total_height(&mut self, width: u16) -> u32 {
        let mut total: u32 = 0;
        for index in 0..self.items.len() {
            total += self.height_cache.get(index, &mut self.items, width) as u32;
        }
        total
    }

    /// Render the visible window into `area`. Every item paints into a scratch buffer
    /// sized to its own full height, then only the rows actually inside the viewport
    /// are copied into `buf` — the mechanism that lets an item straddling the top or
    /// bottom edge render at exact row granularity instead of only whole-item steps.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if area.width == 0 || area.height == 0 || self.items.is_empty() {
            return;
        }
        let width = area.width;
        let total = self.total_height(width);
        let viewport = area.height as u32;
        let max_scroll = total.saturating_sub(viewport);
        if self.sticky_bottom {
            self.scroll_offset = max_scroll;
        } else {
            self.scroll_offset = self.scroll_offset.min(max_scroll);
        }
        let top = self.scroll_offset;
        let bottom = top + viewport;

        let mut item_top: u32 = 0;
        let mut screen_y = area.y;
        for index in 0..self.items.len() {
            let height = self.height_cache.get(index, &mut self.items, width) as u32;
            let item_bottom = item_top + height;
            if height == 0 || item_bottom <= top || item_top >= bottom {
                item_top = item_bottom;
                continue;
            }
            let skip_top = top.saturating_sub(item_top) as u16;
            let available = (bottom.min(item_bottom) - item_top.max(top)) as u16;
            if available > 0 {
                let clip = ClipGeometry {
                    screen_y,
                    skip_top,
                    full_height: height as u16,
                    available,
                };
                paint_clipped(&mut self.items[index], buf, area, clip, theme);
                screen_y += available;
            }
            item_top = item_bottom;
        }
    }
}

/// Where a clipped item lands in `buf`, and which of its own rows are visible.
struct ClipGeometry {
    screen_y: u16,
    skip_top: u16,
    full_height: u16,
    available: u16,
}

/// Paints one item into a `full_height`-tall scratch buffer, then blits the
/// `available` rows starting at its own row `skip_top` into `buf` at `screen_y`.
/// Blitting whole cells (glyph + style) is transparent to plain text; kitty's
/// unicode-placeholder image protocol also survives the move since each cell's
/// diacritic encodes an offset *within the image*, not an absolute screen position.
/// Sixel is more likely to need a mid-image crop to be revisited if a soak test
/// shows artifacts.
fn paint_clipped(
    item: &mut TranscriptItem,
    dest: &mut Buffer,
    area: Rect,
    clip: ClipGeometry,
    theme: &Theme,
) {
    let ClipGeometry {
        screen_y,
        skip_top,
        full_height,
        available,
    } = clip;
    if full_height == available && skip_top == 0 {
        // The common case — the item fits entirely within the viewport — paints
        // straight into the destination buffer, no scratch copy needed.
        let target = Rect::new(area.x, screen_y, area.width, available);
        item.paint(dest, target, theme);
        return;
    }
    let scratch_area = Rect::new(0, 0, area.width, full_height);
    let mut scratch = Buffer::empty(scratch_area);
    item.paint(&mut scratch, scratch_area, theme);
    for row in 0..available {
        for col in 0..area.width {
            if let Some(cell) = scratch.cell((col, skip_top + row)) {
                let cell = cell.clone();
                if let Some(dest_cell) = dest.cell_mut((area.x + col, screen_y + row)) {
                    *dest_cell = cell;
                }
            }
        }
    }
}

#[derive(Default)]
struct HeightCache {
    width: u16,
    entries: Vec<Option<(u64, u16)>>,
}

impl HeightCache {
    fn get(&mut self, index: usize, items: &mut [TranscriptItem], width: u16) -> u16 {
        if width != self.width {
            self.entries.clear();
            self.width = width;
        }
        if self.entries.len() <= index {
            self.entries.resize(items.len(), None);
        }
        let revision = items[index].revision();
        if let Some((cached_revision, height)) = self.entries[index]
            && cached_revision == revision
        {
            return height;
        }
        let height = items[index].height(width);
        self.entries[index] = Some((revision, height));
        height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorCapability, ThemeName};

    fn theme() -> Theme {
        Theme::new(ThemeName::Dark, ColorCapability::TrueColor)
    }

    fn render_to_string(transcript: &mut Transcript, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        transcript.render(&mut buf, area, &theme());
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn a_message_item_renders_its_markdown() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Message(MessageItem::new(
            "m1",
            MessageRole::Assistant,
            "hello **world**",
        )));
        let content = render_to_string(&mut transcript, 40, 10);
        assert!(content.contains("hello"));
        assert!(content.contains("world"));
    }

    #[test]
    fn empty_reasoning_renders_nothing_and_takes_no_height() {
        let mut item = TranscriptItem::Reasoning(ReasoningItem::new("r1", "   "));
        assert_eq!(item.height(80), 0);
    }

    #[test]
    fn reasoning_collapses_to_one_line_and_expands_on_toggle() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Reasoning(ReasoningItem::new(
            "r1",
            "first line\nsecond line of reasoning",
        )));
        let collapsed = render_to_string(&mut transcript, 60, 10);
        assert!(collapsed.contains("Thinking"));
        assert!(collapsed.contains("first line"));
        assert!(!collapsed.contains("second line"));

        transcript.toggle_reasoning("r1");
        let expanded = render_to_string(&mut transcript, 60, 10);
        assert!(expanded.contains("second"));
    }

    #[test]
    fn tool_card_default_open_matches_the_running_execute_with_output_rule() {
        let mut running = ToolItem::new(
            "t1",
            "Bash",
            Some(&serde_json::json!({"command": "npm test"})),
            ToolStatus::Running,
        );
        running.output = Some("installing...".to_owned());
        assert!(running.open());

        let mut finished = ToolItem::new(
            "t2",
            "Bash",
            Some(&serde_json::json!({"command": "npm test"})),
            ToolStatus::Completed,
        );
        finished.output = Some("ok".to_owned());
        assert!(!finished.open());

        let mut failed = ToolItem::new(
            "t3",
            "Read",
            Some(&serde_json::json!({"path": "a.rs"})),
            ToolStatus::Failed,
        );
        failed.error = Some("not found".to_owned());
        assert!(
            !failed.open(),
            "a failure no longer springs open on its own"
        );
    }

    #[test]
    fn toggling_a_tool_card_always_wins_over_the_default() {
        let mut transcript = Transcript::new();
        let mut tool = ToolItem::new(
            "t1",
            "Read",
            Some(&serde_json::json!({"path": "a.rs"})),
            ToolStatus::Completed,
        );
        tool.output = Some("contents".to_owned());
        transcript.push(TranscriptItem::Tool(tool));

        let collapsed = render_to_string(&mut transcript, 60, 10);
        assert!(!collapsed.contains("contents"));

        transcript.toggle_tool("t1");
        let expanded = render_to_string(&mut transcript, 60, 10);
        assert!(expanded.contains("contents"));
    }

    #[test]
    fn a_locked_card_with_no_detail_shows_no_chevron() {
        let card = ToolItem::new(
            "t1",
            "Edit",
            Some(&serde_json::json!({"path": "a.rs"})),
            ToolStatus::Completed,
        );
        assert!(!card.has_detail());
        assert!(!card.open());
    }

    #[test]
    fn note_tones_carry_their_theme_color() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Note(NoteItem::new(
            "n1",
            "a danger note",
            NoteTone::Danger,
        )));
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        transcript.render(&mut buf, area, &theme());
        let cell = buf.cell((0, 0)).expect("first cell painted");
        assert_eq!(cell.fg, theme().palette.failed);
    }

    #[test]
    fn malformed_image_bytes_render_a_placeholder_not_a_panic() {
        let support = ImageSupport::halfblocks();
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Image(ImageItem::decode(
            "img1",
            &support,
            "not base64 image data",
        )));
        let content = render_to_string(&mut transcript, 30, 6);
        assert!(content.contains("image could not be decoded"));
        assert!(content.contains("open externally"));
    }

    #[test]
    fn scroll_offset_is_clamped_and_sticky_bottom_tracks_new_items() {
        let mut transcript = Transcript::new();
        for index in 0..50 {
            transcript.push(TranscriptItem::Note(NoteItem::new(
                format!("n{index}"),
                format!("note {index}"),
                NoteTone::Dim,
            )));
        }
        // Sticky by default: rendering at a small viewport should show the tail.
        let content = render_to_string(&mut transcript, 40, 5);
        assert!(content.contains("note 49"));
        assert!(!content.contains("note 0 "));

        transcript.scroll_by(-1_000_000);
        let content = render_to_string(&mut transcript, 40, 5);
        assert!(content.contains("note 0"));

        transcript.jump_to_bottom();
        let content = render_to_string(&mut transcript, 40, 5);
        assert!(content.contains("note 49"));
    }

    #[test]
    fn resizing_the_width_invalidates_the_whole_height_cache() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Message(MessageItem::new(
            "m1",
            MessageRole::Assistant,
            "a fairly long sentence that will wrap differently at different widths",
        )));
        let narrow_area = Rect::new(0, 0, 20, 20);
        let mut narrow_buf = Buffer::empty(narrow_area);
        transcript.render(&mut narrow_buf, narrow_area, &theme());
        let narrow_height = transcript.height_cache.entries[0].expect("cached").1;

        let wide_area = Rect::new(0, 0, 80, 20);
        let mut wide_buf = Buffer::empty(wide_area);
        transcript.render(&mut wide_buf, wide_area, &theme());
        let wide_height = transcript.height_cache.entries[0].expect("cached").1;

        assert!(narrow_height > wide_height);
    }

    #[test]
    fn an_item_straddling_the_viewport_edge_renders_the_partial_row_correctly() {
        let mut transcript = Transcript::new();
        for index in 0..3 {
            transcript.push(TranscriptItem::Note(NoteItem::new(
                format!("n{index}"),
                format!("note-{index}"),
                NoteTone::Dim,
            )));
        }
        transcript.scroll_by(-1_000_000); // pin to the top
        // A 2-row viewport over 3 one-row notes shows exactly the first two.
        let content = render_to_string(&mut transcript, 20, 2);
        assert!(content.contains("note-0"));
        assert!(content.contains("note-1"));
        assert!(!content.contains("note-2"));
    }

    const SNAPSHOT_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

    /// One of each item kind the A7 accept criterion names — message, reasoning, tool,
    /// image — collapsed or expanded, plus a note for good measure.
    fn snapshot_fixture(expanded: bool) -> Transcript {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Message(MessageItem::new(
            "msg-user",
            MessageRole::User,
            "Add retry logic to the sync job.",
        )));
        transcript.push(TranscriptItem::Message(MessageItem::new(
            "msg-assistant",
            MessageRole::Assistant,
            "I'll add exponential backoff to `sync_job`.",
        )));

        let mut reasoning = ReasoningItem::new(
            "reasoning-1",
            "The job currently retries immediately on failure.\nExponential backoff avoids hammering the upstream API.",
        );
        reasoning.expanded = expanded;
        transcript.push(TranscriptItem::Reasoning(reasoning));

        let edit = ToolItem::new(
            "tool-edit",
            "Edit",
            Some(&serde_json::json!({"file_path": "src/sync_job.rs"})),
            ToolStatus::Completed,
        );
        transcript.push(TranscriptItem::Tool(edit));

        let mut bash = ToolItem::new(
            "tool-bash",
            "Bash",
            Some(
                &serde_json::json!({"command": "cargo test sync_job", "description": "Run the sync job tests"}),
            ),
            ToolStatus::Completed,
        );
        bash.output = Some(
            "running 3 tests\ntest sync_job::tests::retries_on_failure ... ok\ntest result: ok. 3 passed"
                .to_owned(),
        );
        bash.exit_code = Some(0);
        bash.user_expanded = Some(expanded);
        transcript.push(TranscriptItem::Tool(bash));

        transcript.push(TranscriptItem::Note(NoteItem::new(
            "note-1",
            "session resumed after usage limit",
            NoteTone::Warning,
        )));

        let support = ImageSupport::halfblocks();
        transcript.push(TranscriptItem::Image(ImageItem::decode(
            "image-ok",
            &support,
            SNAPSHOT_PNG_BASE64,
        )));
        transcript.push(TranscriptItem::Image(ImageItem::decode(
            "image-bad",
            &support,
            "not base64",
        )));

        transcript
    }

    fn snapshot_at(expanded: bool, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut transcript = snapshot_fixture(expanded);
        transcript.scroll_by(-1_000_000); // pin to the top: show every fixture item, not the tail
        let area = Rect::new(0, 0, width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| transcript.render(frame.buffer_mut(), area, &theme()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn snapshot_collapsed_states_at_three_sizes() {
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            insta::assert_debug_snapshot!(
                format!("transcript_collapsed_{width}x{height}"),
                snapshot_at(false, width, height)
            );
        }
    }

    #[test]
    fn snapshot_expanded_states_at_three_sizes() {
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            insta::assert_debug_snapshot!(
                format!("transcript_expanded_{width}x{height}"),
                snapshot_at(true, width, height)
            );
        }
    }
}
