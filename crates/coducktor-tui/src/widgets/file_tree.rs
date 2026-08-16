//! The file tree pane shared by the Changes tab (task git and repo git) — spec §8.5's
//! "file tree/list on the left", built from a structured diff's changed-file paths.
//!
//! Hand-rolled rather than `tui-tree-widget` — spec §6.1 names that crate's own listed
//! fallback for exactly this widget, and `tui-tree-widget` 0.24's `Tree` implements
//! `ratatui_core::widgets::StatefulWidget`, a trait `ratatui` 0.29 (this workspace's pinned
//! version, fixed at A0 for `Paragraph::line_count`) does not accept from `Frame::
//! render_stateful_widget`: the two crates' widget-trait generations don't line up. A flat,
//! indented row list — the same shape the diff widget itself already renders as `Line`s —
//! needs nothing from that ecosystem.

use std::collections::BTreeMap;

use coducktor_contract::{ChangedFile, ChangedFileStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// One flattened, indentation-ready row: a folder header or a changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeRow {
    /// Full path from the tree root — a folder's is its own path, a file's is `ChangedFile.path`.
    /// This is the row's own identity (for fold/collapse tracking), NOT the diff widget's file
    /// key — a renamed file's diff-widget identity also carries its `oldPath` (see `file_key`).
    pub path: String,
    /// Present only on a file row: the same identity `diff::file_key` computes for this file,
    /// so a screen can drive `DiffViewState::toggle_file` from a tree selection without the two
    /// widgets disagreeing about a renamed file's key.
    pub file_key: Option<String>,
    pub depth: u16,
    pub label: String,
    pub kind: FileTreeRowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTreeRowKind {
    Folder,
    File(ChangedFileStatus),
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    file: Option<(ChangedFileStatus, String)>,
}

/// Build the folder/file hierarchy for a set of changed files and flatten it into rows honoring
/// `collapsed` (folder paths currently closed) — mirrors `changes-tree.tsx`'s `buildFileTree`
/// plus its own flatten-for-render step, without a virtual-DOM tree to keep state in.
pub fn build_rows(
    files: &[ChangedFile],
    collapsed: &std::collections::HashSet<String>,
) -> Vec<FileTreeRow> {
    let mut root = Node::default();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut node = &mut root;
        for (index, part) in parts.iter().enumerate() {
            node = node.children.entry((*part).to_owned()).or_default();
            if index == parts.len() - 1 {
                node.file = Some((file.status, crate::diff::file_key(file)));
            }
        }
    }

    let mut rows = Vec::new();
    fn walk(
        name: &str,
        path: &str,
        node: &Node,
        depth: u16,
        collapsed: &std::collections::HashSet<String>,
        rows: &mut Vec<FileTreeRow>,
    ) {
        if node.children.is_empty() {
            let (status, file_key) = node
                .file
                .clone()
                .unwrap_or((ChangedFileStatus::Modified, path.to_owned()));
            rows.push(FileTreeRow {
                path: path.to_owned(),
                file_key: Some(file_key),
                depth,
                label: name.to_owned(),
                kind: FileTreeRowKind::File(status),
            });
            return;
        }
        rows.push(FileTreeRow {
            path: path.to_owned(),
            file_key: None,
            depth,
            label: name.to_owned(),
            kind: FileTreeRowKind::Folder,
        });
        if collapsed.contains(path) {
            return;
        }
        for (child_name, child) in &node.children {
            let child_path = format!("{path}/{child_name}");
            walk(child_name, &child_path, child, depth + 1, collapsed, rows);
        }
    }
    for (name, node) in &root.children {
        walk(name, name, node, 0, collapsed, &mut rows);
    }
    rows
}

fn status_letter(status: ChangedFileStatus) -> char {
    match status {
        ChangedFileStatus::Added => 'A',
        ChangedFileStatus::Deleted => 'D',
        ChangedFileStatus::Modified => 'M',
        ChangedFileStatus::Renamed => 'R',
        ChangedFileStatus::Copied => 'C',
    }
}

fn status_color(status: ChangedFileStatus, theme: &Theme) -> ratatui::style::Color {
    match status {
        ChangedFileStatus::Added => theme.palette.add,
        ChangedFileStatus::Deleted => theme.palette.del,
        ChangedFileStatus::Modified | ChangedFileStatus::Renamed | ChangedFileStatus::Copied => {
            theme.palette.soft_fg
        }
    }
}

/// Render one row as a themed `Line`, `selected` reversing it the same way every other list in
/// this app highlights its current row (`widgets/table.rs`'s convention).
pub fn render_row(row: &FileTreeRow, theme: &Theme, selected: bool) -> Line<'static> {
    let indent = "  ".repeat(row.depth as usize);
    let mut style = Style::default().fg(theme.palette.fg);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let spans = match row.kind {
        FileTreeRowKind::Folder => vec![
            Span::styled(indent, style),
            Span::styled("▸ ", style.fg(theme.palette.soft_fg)),
            Span::styled(row.label.clone(), style.add_modifier(Modifier::BOLD)),
        ],
        FileTreeRowKind::File(status) => vec![
            Span::styled(indent, style),
            Span::styled(
                format!("{} ", status_letter(status)),
                style.fg(status_color(status, theme)),
            ),
            Span::styled(row.label.clone(), style),
        ],
    };
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> ChangedFile {
        ChangedFile {
            path: path.to_owned(),
            old_path: None,
            status: ChangedFileStatus::Modified,
            adds: 1.0,
            dels: 0.0,
            binary: false,
            image: None,
            patch: String::new(),
        }
    }

    #[test]
    fn groups_files_under_shared_folders() {
        let files = vec![file("src/a.rs"), file("src/b.rs"), file("README.md")];
        let rows = build_rows(&files, &Default::default());
        // README.md (leaf), src (folder), src/a.rs, src/b.rs — BTreeMap orders "README.md" < "src".
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].path, "README.md");
        assert_eq!(rows[1].kind, FileTreeRowKind::Folder);
        assert_eq!(rows[2].path, "src/a.rs");
        assert_eq!(rows[2].depth, 1);
    }

    #[test]
    fn a_collapsed_folder_hides_its_children() {
        let files = vec![file("src/a.rs"), file("src/b.rs")];
        let collapsed = std::collections::HashSet::from(["src".to_owned()]);
        let rows = build_rows(&files, &collapsed);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, FileTreeRowKind::Folder);
    }

    #[test]
    fn a_single_top_level_file_is_a_leaf() {
        let files = vec![file("Cargo.toml")];
        let rows = build_rows(&files, &Default::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].kind,
            FileTreeRowKind::File(ChangedFileStatus::Modified)
        );
    }
}
