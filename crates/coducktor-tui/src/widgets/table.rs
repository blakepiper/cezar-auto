//! The shared column table widget behind the Tasks and Global tasks screens.
//!
//! The widget owns layout, folding, selection, scrolling and hit registration;
//! the screens own the data, the ordering and the actions.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::input::hitmap::{HitAction, HitMap};

/// The width of a folded column.
pub const FOLDED_WIDTH: u16 = 3;
/// Rows rendered below the header line.
pub const HEADER_HEIGHT: u16 = 1;

/// The columns a run table can show. `Project` only appears on the global screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColumnId {
    Status,
    Task,
    Workflow,
    Branch,
    Diff,
    Reference,
    Tokens,
    Cost,
    Cpu,
    Memory,
    Started,
    Project,
    Tag,
}

impl ColumnId {
    pub fn title(self) -> &'static str {
        match self {
            Self::Status => "STATUS",
            Self::Task => "TASK",
            Self::Workflow => "WORKFLOW",
            Self::Branch => "BRANCH",
            Self::Diff => "±",
            Self::Reference => "REF",
            Self::Tokens => "IN/OUT",
            Self::Cost => "COST",
            Self::Cpu => "CPU",
            Self::Memory => "MEM",
            Self::Started => "STARTED",
            Self::Project => "PROJECT",
            Self::Tag => "TAG",
        }
    }

    /// The marker shown while a column is folded.
    pub fn folded_label(self) -> &'static str {
        match self {
            Self::Status => "st",
            Self::Task => "ta",
            Self::Workflow => "wf",
            Self::Branch => "br",
            Self::Diff => "±",
            Self::Reference => "ref",
            Self::Tokens => "io",
            Self::Cost => "$",
            Self::Cpu => "cpu",
            Self::Memory => "mem",
            Self::Started => "↑",
            Self::Project => "pr",
            Self::Tag => "tag",
        }
    }

    /// The preferred expanded width in cells. `Task` returns 0: it is flexible.
    pub fn width(self) -> u16 {
        match self {
            Self::Status => 10,
            Self::Task => 0,
            Self::Workflow => 10,
            Self::Branch => 13,
            Self::Diff => 8,
            Self::Reference => 8,
            Self::Tokens => 13,
            Self::Cost => 8,
            Self::Cpu => 7,
            Self::Memory => 9,
            Self::Started => 9,
            Self::Project => 12,
            Self::Tag => 12,
        }
    }

    pub fn foldable(self) -> bool {
        !matches!(self, Self::Status | Self::Task)
    }

    pub fn right(self) -> bool {
        matches!(
            self,
            Self::Tokens | Self::Cost | Self::Cpu | Self::Memory | Self::Started
        )
    }
}

/// How the screen ordered the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub column: ColumnId,
    pub descending: bool,
}

/// One rendered cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub text: String,
    pub style: Style,
}

impl TableCell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub fn empty(style: Style) -> Self {
        Self {
            text: String::new(),
            style,
        }
    }
}

/// One rendered row; `cells` must have exactly one entry per visible column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub key: String,
    pub cells: Vec<TableCell>,
}

/// The widget state a screen owns and mutates.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<ColumnId>,
    pub folded: std::collections::BTreeSet<ColumnId>,
    pub rows: Vec<TableRow>,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    pub scroll_y: usize,
    pub first_column: usize,
    pub sort: Option<SortState>,
    /// The header-column rectangles from the last render, for hover folding.
    pub header_hits: Vec<(ColumnId, Rect)>,
}

impl Table {
    pub fn with_columns(columns: Vec<ColumnId>) -> Self {
        Self {
            columns,
            ..Self::default()
        }
    }

    /// The index of the selected row, clamped to the current list.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&index| index < self.rows.len());
    }

    /// The selection index with its row key, if any.
    pub fn selected_row(&self) -> Option<(usize, &TableRow)> {
        self.selected.map(|index| (index, &self.rows[index]))
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.rows.len();
        if len == 0 {
            self.selected = None;
            return;
        }
        let next = if self.selected.is_none() {
            if delta > 0 { 0 } else { len - 1 }
        } else {
            let current = self.selected.unwrap_or(0);
            (current as isize + delta).clamp(0, len as isize - 1) as usize
        };
        self.select(Some(next));
        self.keep_selection_visible();
    }

    fn keep_selection_visible(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        if selected < self.scroll_y {
            self.scroll_y = selected;
        }
    }

    /// Toggle a foldable column's folded state.
    pub fn toggle_fold(&mut self, column: ColumnId) {
        if column.foldable() && !self.folded.remove(&column) {
            self.folded.insert(column);
        }
    }

    pub fn is_folded(&self, column: ColumnId) -> bool {
        self.folded.contains(&column)
    }

    pub fn effective_width(&self, column: ColumnId) -> u16 {
        if self.is_folded(column) {
            FOLDED_WIDTH
        } else {
            column.width()
        }
    }

    /// Scroll the horizontal window one column left/right.
    pub fn scroll_columns(&mut self, delta: isize) {
        let max = self.columns.len().saturating_sub(1);
        let current = self.first_column;
        let next = (current as isize + delta).clamp(0, max as isize) as usize;
        self.first_column = next;
    }

    /// Layout the visible column slice for a given area width. Returns
    /// `(absolute_column_index, column, width)` triples; the `Task` column is
    /// flexible in its declared position, and columns that cannot fit are
    /// dropped from the right so rows never overflow the area.
    fn layout_columns(&self, width: u16) -> Vec<(usize, ColumnId, u16)> {
        let mut columns: Vec<(usize, ColumnId, u16)> = self
            .columns
            .iter()
            .enumerate()
            .skip(self.first_column)
            .map(|(index, column)| {
                (
                    index,
                    *column,
                    if *column == ColumnId::Task {
                        0
                    } else {
                        self.effective_width(*column)
                    },
                )
            })
            .collect();
        let has_task = columns
            .iter()
            .any(|(_, column, _)| *column == ColumnId::Task);
        let min_width = if has_task { 9 } else { 0 }; // task min + separator
        // Fixed width of every non-flexible column plus its separator.
        let mut fixed: u16 = columns
            .iter()
            .filter(|(_, column, _)| *column != ColumnId::Task)
            .map(|(_, _, column_width)| column_width.saturating_add(1))
            .sum();
        // Trim non-flexible columns from the right until the remainder fits.
        while fixed.saturating_add(min_width) > width {
            let Some(position) = columns
                .iter()
                .rposition(|(_, column, _)| *column != ColumnId::Task)
            else {
                break;
            };
            fixed = fixed.saturating_sub(columns[position].2.saturating_add(1));
            columns.remove(position);
        }
        if has_task
            && let Some(task) = columns
                .iter_mut()
                .find(|(_, column, _)| *column == ColumnId::Task)
        {
            task.2 = width.saturating_sub(fixed).max(8);
        }
        columns
    }

    /// Render into the area, registering hits on `hitmap`. `empty_hint` is shown
    /// when there are no rows.
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        hitmap: &mut HitMap,
        title: &str,
        empty_hint: &str,
    ) {
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        let body_height = inner.height.saturating_sub(HEADER_HEIGHT);
        let total = self.rows.len();
        let max_scroll = total.saturating_sub(body_height as usize);
        self.scroll_y = self.scroll_y.min(max_scroll);
        self.select(self.selected);

        let columns = self.layout_columns(inner.width);
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(self.header_line(&columns));
        let start = self.scroll_y;
        let end = (start + body_height as usize).min(total);
        for index in start..end {
            lines.push(self.row_line(index, &columns));
        }
        if total == 0 {
            lines.push(Line::from(Span::raw(empty_hint.to_owned())));
        }

        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines), inner);

        // Hit registration, aligned with the rendered geometry.
        let mut column_x = inner.x;
        self.header_hits.clear();
        for (index, column, column_width) in &columns {
            let rect = Rect::new(column_x, inner.y, *column_width, HEADER_HEIGHT);
            self.header_hits.push((*column, rect));
            if column.foldable() {
                hitmap.register(rect, 3, HitAction::TableHeader(*column));
            }
            let _ = index;
            column_x = column_x.saturating_add(*column_width).saturating_add(1);
        }
        let mut row_y = inner.y.saturating_add(HEADER_HEIGHT);
        for index in start..end {
            let rect = Rect::new(inner.x, row_y, inner.width, 1);
            hitmap.register(rect, 2, HitAction::TableRow(index));
            row_y = row_y.saturating_add(1);
        }
    }

    fn header_line(&self, columns: &[(usize, ColumnId, u16)]) -> Line<'static> {
        let style = Style::default().add_modifier(Modifier::BOLD);
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (_, column, width) in columns {
            let label = if self.is_folded(*column) {
                column.folded_label().to_owned()
            } else {
                truncate(column.title(), *width as usize)
            };
            let mut text = if column.right() {
                pad_left(&label, *width as usize)
            } else {
                label
            };
            if let Some(sort) = self.sort
                && sort.column == *column
            {
                text.push(if sort.descending { '▼' } else { '▲' });
            }
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    }

    fn row_line(&self, index: usize, columns: &[(usize, ColumnId, u16)]) -> Line<'static> {
        let row = &self.rows[index];
        let selected = self.selected == Some(index);
        let hovered = self.hover == Some(index);
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (column_index, column, width) in columns {
            let cell = row
                .cells
                .get(*column_index)
                .cloned()
                .unwrap_or_else(|| TableCell::empty(Style::default()));
            let mut style = cell.style;
            if selected || hovered {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let text = truncate(&cell.text, *width as usize);
            let text = if column.right() {
                pad_left(&text, *width as usize)
            } else {
                text
            };
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    }
}

fn pad_left(text: &str, width: usize) -> String {
    let missing = width.saturating_sub(text.chars().count());
    format!("{}{}", " ".repeat(missing), text)
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() && max > 1 {
        result.pop();
        result.push('~');
    }
    result
}
