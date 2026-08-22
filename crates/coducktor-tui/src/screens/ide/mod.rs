//! The IDE screen. Left: the explorer; right: the editor with
//! syntect highlighting, line numbers, a dirty indicator, `Ctrl+S` save and `Ctrl+E`'s
//! "open in $EDITOR" escape hatch (spawned by `main.rs`, which owns the terminal).
//!
//! The engine owns the safety rules — 1 MB edit cap, `.git` exclusion, symlink exclusion —
//! and this screen honors them and explains them in footer hints. The unsaved-changes guard is
//! `App::request_navigate`/`request_back`,
//! which turns any navigation away from a dirty file into a confirm dialog.

use coducktor_contract::{IdeDirectoryResponse, IdeEntry, IdeEntryType};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, PendingAction, Route};
use crate::diff::Highlighter;
use crate::input::hitmap::{HitAction, IdeAction};
use crate::theme::Theme;
use crate::widgets::editor::Editor;

/// The IDE's safety rules, explained to the user.
const EXPLORER_FOOTER: &str = ".git and symlinks are hidden";
const EDITOR_FOOTER: &str = "UTF-8 · Ctrl+S save · Ctrl+E open in $EDITOR · 1 MB edit cap";

/// Engine-fetched state for the open IDE screen.
pub struct IdeUi {
    pub project: String,
    /// Explorer directory — the empty string is the project root.
    pub directory_path: String,
    pub entries: Option<IdeDirectoryResponse>,
    pub tree_selected: usize,
    pub focus: IdeFocus,
    /// Bumped by every directory load dispatch. Matching `directory_path` alone cannot tell a
    /// slow, superseded load of the *same* directory (a re-entered folder, or a round trip
    /// through another one and back) from the current one — background results arrive off a
    /// multi-worker pool with no ordering guarantee, so an older request for an identical path
    /// can complete after a newer one.
    pub directory_generation: u64,

    /// The open file. `None` means the editor pane is idle.
    pub file_path: Option<String>,
    pub file_size: u64,
    /// Bumped by every file load dispatch — the file analog of `directory_generation`, needed
    /// for the same reason: reopening the same path (directly, or via A → B → A navigation)
    /// dispatches a new load whose result must win over a still-outstanding older one.
    pub file_generation: u64,
    /// The reason a file could not be opened (too large / binary / symlink /
    /// missing) — rendered verbatim, then the pane explains the cap.
    pub file_error: Option<String>,
    pub editor: Editor,
    pub dirty: bool,

    /// The editor pane's viewport height at the last render — what page-scroll math
    /// moves against, so it always matches the screen.
    pub last_viewport: usize,

    pub highlighter: Highlighter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeFocus {
    Tree,
    Editor,
}

impl Default for IdeUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            directory_path: String::new(),
            entries: None,
            tree_selected: 0,
            focus: IdeFocus::Tree,
            directory_generation: 0,
            file_path: None,
            file_size: 0,
            file_generation: 0,
            file_error: None,
            editor: Editor::default(),
            dirty: false,
            last_viewport: 20,
            highlighter: Highlighter::new(),
        }
    }
}

impl IdeUi {
    /// Start a new directory load, invalidating any still-outstanding one for the same path.
    pub fn begin_directory_request(&mut self) -> u64 {
        self.directory_generation = self.directory_generation.wrapping_add(1);
        self.directory_generation
    }

    /// Start a new file load, invalidating any still-outstanding one for the same path.
    pub fn begin_file_request(&mut self) -> u64 {
        self.file_generation = self.file_generation.wrapping_add(1);
        self.file_generation
    }
}

impl IdeUi {
    /// Drop the draft and the open file — the "discard" half of the unsaved-changes guard.
    pub fn discard(&mut self) {
        self.dirty = false;
        self.file_path = None;
        self.file_size = 0;
        self.file_error = None;
        self.editor = Editor::default();
        self.focus = IdeFocus::Tree;
    }
}

/// Navigate to the IDE; `navigate_route` owns the project sync and the root
/// listing queue, so this is just the route hop.
pub fn open(app: &mut App, project: &str) {
    app.request_navigate(Route::Ide {
        project: project.to_owned(),
    });
}

fn parent_path(path: &str) -> String {
    let slash = path.rfind('/');
    slash
        .map(|index| path[..index].to_owned())
        .unwrap_or_default()
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    render_header(frame, rows[0], app);

    let (explorer_area, editor_area) = if rows[1].width < 60 {
        (Rect::new(rows[1].x, rows[1].y, 0, 0), rows[1])
    } else {
        let explorer_width = (rows[1].width / 3).clamp(24, 36);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(explorer_width), Constraint::Min(1)])
            .split(rows[1]);
        (cols[0], cols[1])
    };
    if explorer_area.width > 0 {
        render_explorer(frame, explorer_area, app);
    }
    render_editor_pane(frame, editor_area, app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let path = app.ide_ui.directory_path.as_str();
    let title = if path.is_empty() {
        "IDE — project root".to_owned()
    } else {
        format!("IDE — /{path}")
    };
    let title_len = title.chars().count() as u16;
    let mut spans = vec![Span::styled(
        title,
        Style::default()
            .fg(app.theme.palette.fg)
            .add_modifier(Modifier::BOLD),
    )];
    if !path.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[.. up]",
            Style::default().fg(app.theme.palette.soft_fg),
        ));
        app.hitmap.register(
            Rect::new(area.x + title_len + 2, area.y, 8, 1),
            3,
            HitAction::IdeScreen(IdeAction::GoUp),
        );
    }
    if app
        .ide_ui
        .entries
        .as_ref()
        .is_some_and(|directory| directory.truncated)
    {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "folder too large to show every entry",
            Style::default().fg(app.theme.palette.waiting),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_explorer(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title("Files");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    match app.ide_ui.entries.clone() {
        None => lines.push(Line::from(Span::styled(
            "Loading…",
            Style::default().fg(app.theme.palette.soft_fg),
        ))),
        Some(directory) if directory.entries.is_empty() => {
            lines.push(Line::from(Span::styled(
                "This folder is empty.",
                Style::default().fg(app.theme.palette.soft_fg),
            )));
        }
        Some(directory) => {
            let selected = app
                .ide_ui
                .tree_selected
                .min(directory.entries.len().saturating_sub(1));
            for (index, entry) in directory.entries.iter().enumerate() {
                lines.push(entry_line(entry, &app.theme, index == selected));
                // One row above the footer, so the explainer line is never a hit target.
                if let Some(y) = inner.y.checked_add(index as u16)
                    && y + 1 < inner.bottom()
                {
                    app.hitmap.register(
                        Rect::new(inner.x, y, inner.width, 1),
                        2,
                        HitAction::IdeScreen(IdeAction::SelectEntry(index)),
                    );
                }
            }
            app.ide_ui.tree_selected = selected;
            if directory.truncated {
                lines.push(Line::from(Span::styled(
                    "… folder too large to show every entry",
                    Style::default().fg(app.theme.palette.waiting),
                )));
            }
        }
    }
    // Reserve the footer row for the explainer; anything longer is clipped.
    let inner_height = inner.height.saturating_sub(1);
    lines.truncate(inner_height as usize);
    frame.render_widget(Paragraph::new(lines), inner);

    let footer_y = inner.bottom().saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            EXPLORER_FOOTER,
            Style::default().fg(app.theme.palette.soft_fg),
        ))),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

fn entry_line(entry: &IdeEntry, theme: &Theme, selected: bool) -> Line<'static> {
    let icon = match entry.entry_type {
        IdeEntryType::Dir => "▸ ",
        IdeEntryType::File => "  ",
    };
    let mut style = Style::default().fg(theme.palette.fg);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let mut spans = vec![Span::styled(format!("{icon}{}", entry.name), style)];
    if entry.entry_type == IdeEntryType::File
        && let Some(size) = entry.size
    {
        spans.push(Span::styled(
            format!("  {}", format_bytes(size)),
            Style::default().fg(theme.palette.soft_fg),
        ));
    }
    Line::from(spans)
}

fn format_bytes(size: u64) -> String {
    if size < 1_024 {
        format!("{size} B")
    } else if size < 1_024 * 1_024 {
        format!("{} KB", size / 1_024)
    } else {
        format!("{:.1} MB", size as f64 / (1_024.0 * 1_024.0))
    }
}

fn render_editor_pane(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = match app.ide_ui.file_path.as_deref() {
        Some(path) if app.ide_ui.dirty => format!("{path}  ● unsaved"),
        Some(path) => format!("{path}  ✓ saved"),
        None => "Editor".to_owned(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // One inner row is the footer; the editor viewport is what sits above it.
    app.ide_ui.last_viewport = inner.height.saturating_sub(1) as usize;
    let Some(path) = app.ide_ui.file_path.clone() else {
        frame.render_widget(
            Paragraph::new("Choose a file from the explorer to start editing.")
                .style(Style::default().fg(app.theme.palette.soft_fg)),
            inner,
        );
        return;
    };
    if let Some(error) = &app.ide_ui.file_error {
        let text = format!(
            "{error}\n\nThe editor is capped at 1 MB; files above it, binary files, symlinks \
             and .git internals cannot be edited here. Use Ctrl+E to open this file in $EDITOR."
        );
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(app.theme.palette.failed))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let focused = app.ide_ui.focus == IdeFocus::Editor;
    let lines = app.ide_ui.editor.render_lines(
        &path,
        &app.ide_ui.highlighter,
        &app.theme,
        app.ide_ui.last_viewport,
        focused,
    );
    frame.render_widget(Paragraph::new(lines), inner);

    // The safety-rules explainer footer.
    let footer_y = inner.bottom().saturating_sub(1);
    let hint = format!("{EDITOR_FOOTER} · {}", format_bytes(app.ide_ui.file_size));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(app.theme.palette.soft_fg),
        )))
        .wrap(Wrap { trim: true }),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match app.ide_ui.focus {
        IdeFocus::Tree => handle_tree_key(app, key),
        IdeFocus::Editor => handle_editor_key(app, key),
    }
}

pub(crate) fn jump_tree(app: &mut App, end: bool) {
    let last = app
        .ide_ui
        .entries
        .as_ref()
        .map(|directory| directory.entries.len().saturating_sub(1))
        .unwrap_or(0);
    app.ide_ui.tree_selected = if end { last } else { 0 };
}

fn handle_tree_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let count = app
                .ide_ui
                .entries
                .as_ref()
                .map(|directory| directory.entries.len())
                .unwrap_or(0);
            if count > 0 {
                app.ide_ui.tree_selected = (app.ide_ui.tree_selected + 1).min(count - 1);
            }
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.ide_ui.tree_selected = app.ide_ui.tree_selected.saturating_sub(1);
            true
        }
        KeyCode::Char('l') | KeyCode::Enter | KeyCode::Right => {
            open_selected(app);
            true
        }
        KeyCode::Char('h') | KeyCode::Left => {
            apply_hit(app, IdeAction::GoUp);
            true
        }
        _ => false,
    }
}

fn handle_editor_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('s') => {
                apply_hit(app, IdeAction::Save);
                true
            }
            KeyCode::Char('e') => {
                apply_hit(app, IdeAction::OpenInEditor);
                true
            }
            _ => false,
        };
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let ui = &mut app.ide_ui;
    let viewport = ui.last_viewport;
    // Editing keys dirty the file; movement keys do not.
    match key.code {
        KeyCode::Char(character) => {
            ui.editor.insert_char(character);
            ui.dirty = true;
        }
        KeyCode::Enter => {
            ui.editor.insert_newline();
            ui.dirty = true;
        }
        KeyCode::Backspace => {
            ui.editor.backspace();
            ui.dirty = true;
        }
        KeyCode::Delete => {
            ui.editor.delete_forward();
            ui.dirty = true;
        }
        KeyCode::Left => ui.editor.move_left(),
        KeyCode::Right => ui.editor.move_right(),
        KeyCode::Up => ui.editor.move_up(),
        KeyCode::Down => ui.editor.move_down(),
        KeyCode::Home => ui.editor.move_home(),
        KeyCode::End => ui.editor.move_end(),
        KeyCode::PageUp => ui.editor.move_pages(-1, viewport),
        KeyCode::PageDown => ui.editor.move_pages(1, viewport),
        KeyCode::Tab => {
            for _ in 0..4 {
                ui.editor.insert_char(' ');
            }
            ui.dirty = true;
        }
        KeyCode::Esc => {
            ui.focus = IdeFocus::Tree;
        }
        _ => return false,
    }
    ui.editor.ensure_caret_visible(viewport);
    true
}

pub fn apply_hit(app: &mut App, action: IdeAction) {
    match action {
        IdeAction::SelectEntry(index) => {
            app.ide_ui.tree_selected = index;
            open_selected(app);
        }
        IdeAction::GoUp => {
            let parent = parent_path(&app.ide_ui.directory_path);
            app.ide_ui.directory_path = parent.clone();
            let project = app.ide_ui.project.clone();
            app.pending.push(PendingAction::LoadIdeDirectory {
                project,
                path: Some(parent),
            });
        }
        IdeAction::SwitchFocus => {
            if app.ide_ui.file_path.is_some() {
                app.ide_ui.focus = match app.ide_ui.focus {
                    IdeFocus::Tree => IdeFocus::Editor,
                    IdeFocus::Editor => IdeFocus::Tree,
                };
            }
        }
        IdeAction::Save => {
            let Some(path) = app.ide_ui.file_path.clone() else {
                app.notice = Some("no file open to save".to_owned());
                return;
            };
            if !app.ide_ui.dirty {
                app.notice = Some("nothing to save".to_owned());
                return;
            }
            let project = app.ide_ui.project.clone();
            app.pending
                .push(PendingAction::SaveIdeFile { project, path });
        }
        IdeAction::OpenInEditor => {
            let Some(path) = app.ide_ui.file_path.clone() else {
                app.notice = Some("no file open — select one first".to_owned());
                return;
            };
            let project = app.ide_ui.project.clone();
            app.pending
                .push(PendingAction::OpenIdeInEditor { project, path });
        }
    }
}

fn open_selected(app: &mut App) {
    let Some(entry) = app
        .ide_ui
        .entries
        .as_ref()
        .and_then(|directory| directory.entries.get(app.ide_ui.tree_selected).cloned())
    else {
        return;
    };
    match entry.entry_type {
        IdeEntryType::Dir => {
            app.ide_ui.directory_path = entry.path.clone();
            let project = app.ide_ui.project.clone();
            app.pending.push(PendingAction::LoadIdeDirectory {
                project,
                path: Some(entry.path),
            });
        }
        IdeEntryType::File => {
            app.ide_ui.file_path = Some(entry.path.clone());
            app.ide_ui.file_size = entry.size.unwrap_or(0);
            app.ide_ui.file_error = None;
            app.ide_ui.editor.set_text("");
            app.ide_ui.dirty = false;
            app.ide_ui.focus = IdeFocus::Editor;
            let project = app.ide_ui.project.clone();
            app.pending.push(PendingAction::LoadIdeFile {
                project,
                path: entry.path,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app_with_entries() -> App {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.ide_ui.entries = Some(IdeDirectoryResponse {
            path: String::new(),
            entries: vec![
                IdeEntry {
                    name: "src".to_owned(),
                    path: "src".to_owned(),
                    entry_type: IdeEntryType::Dir,
                    size: None,
                },
                IdeEntry {
                    name: "README.md".to_owned(),
                    path: "README.md".to_owned(),
                    entry_type: IdeEntryType::File,
                    size: Some(1_234),
                },
            ],
            truncated: true,
        });
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn explorer_renders_entries_sizes_and_the_truncation_and_symlink_explainers() {
        let mut app = app_with_entries();
        let content = render(&mut app, 120, 40);
        assert!(content.contains("src"));
        assert!(content.contains("README.md"));
        assert!(content.contains("1 KB"), "size shown next to files");
        assert!(content.contains("folder too large to show every entry"));
        assert!(content.contains(".git and symlinks are hidden"));
        assert!(content.contains("IDE — project root"));
    }

    #[test]
    fn opening_a_file_focuses_the_editor_and_shows_the_draft() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        assert_eq!(app.ide_ui.file_path.as_deref(), Some("README.md"));
        assert_eq!(app.ide_ui.focus, IdeFocus::Editor);
        app.ide_ui.editor.set_text("fn main() {}\n");
        app.ide_ui.dirty = false;
        let content = render(&mut app, 120, 40);
        assert!(content.contains("README.md"));
        assert!(content.contains("fn main()"));
        assert!(content.contains("Ctrl+S save"));
        assert!(content.contains("1 MB edit cap"));
    }

    #[test]
    fn ctrl_e_queues_the_external_editor_handoff() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        app.pending.clear();
        app.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.pending.iter().any(|action| {
            matches!(action, PendingAction::OpenIdeInEditor { project, path } if project == "main" && path == "README.md")
        }));
    }

    #[test]
    fn opening_a_file_queues_the_load_for_the_ides_project() {
        let mut app = app_with_entries();
        app.ide_ui.project = "blarchy".to_owned();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        assert!(app.pending.iter().any(|action| {
            matches!(action, PendingAction::LoadIdeFile { project, path } if project == "blarchy" && path == "README.md")
        }));
    }

    #[test]
    fn typing_marks_the_draft_dirty_but_moving_the_caret_does_not() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        assert!(!app.ide_ui.dirty);
        handle_editor_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(app.ide_ui.dirty);
        apply_hit(&mut app, IdeAction::Save);
        assert!(app.pending.iter().any(|action| {
            matches!(action, PendingAction::SaveIdeFile { path, .. } if path == "README.md")
        }));

        // A fresh draft + pure movement stays clean.
        app.ide_ui.editor.set_text("");
        app.ide_ui.dirty = false;
        handle_editor_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_editor_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(!app.ide_ui.dirty, "movement must not dirty the file");
    }

    #[test]
    fn a_too_large_file_renders_the_reason_and_the_cap_explainer() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        app.ide_ui.file_error = Some("file is too large to edit".to_owned());
        let content = render(&mut app, 120, 40);
        assert!(content.contains("file is too large to edit"));
        assert!(content.contains("capped at 1 MB"));
        assert!(content.contains("$EDITOR"));
    }

    #[test]
    fn navigating_away_from_a_dirty_file_asks_before_discarding() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        app.ide_ui.editor.insert_char('x');
        app.ide_ui.dirty = true;
        assert!(app.confirm.is_none());
        app.request_navigate(Route::Tasks {
            project: "main".to_owned(),
        });
        let confirm = app.confirm.clone().expect("guard opens a confirm");
        assert!(matches!(
            confirm.action,
            PendingAction::IdeDiscardThenNavigate(_)
        ));
        assert!(
            matches!(app.route(), Route::Ide { .. }),
            "not navigated yet"
        );
        // Denying keeps the draft.
        app.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        assert!(app.ide_ui.dirty);
        // Confirming navigates and drops it.
        app.request_navigate(Route::Tasks {
            project: "main".to_owned(),
        });
        app.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(app.route(), Route::Tasks { .. }));
        assert!(!app.ide_ui.dirty);
    }

    #[test]
    fn a_clean_file_navigates_away_without_asking() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        app.request_navigate(Route::Tasks {
            project: "main".to_owned(),
        });
        assert!(app.confirm.is_none());
        assert!(matches!(app.route(), Route::Tasks { .. }));
    }

    #[test]
    fn go_up_walks_to_the_parent_directory() {
        let mut app = app_with_entries();
        app.ide_ui.directory_path = "src/lib/deep".to_owned();
        apply_hit(&mut app, IdeAction::GoUp);
        assert_eq!(app.ide_ui.directory_path, "src/lib");
        apply_hit(&mut app, IdeAction::GoUp);
        assert_eq!(app.ide_ui.directory_path, "src");
        apply_hit(&mut app, IdeAction::GoUp);
        assert_eq!(app.ide_ui.directory_path, "");
        apply_hit(&mut app, IdeAction::GoUp);
        assert_eq!(app.ide_ui.directory_path, "");
    }

    #[test]
    fn vim_horizontal_motions_match_tree_arrow_navigation() {
        let mut vim = app_with_entries();
        vim.pending.clear();
        vim.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::NONE,
        )));
        assert_eq!(vim.ide_ui.directory_path, "src");
        assert!(vim.pending.iter().any(|action| {
            matches!(action, PendingAction::LoadIdeDirectory { path: Some(path), .. } if path == "src")
        }));

        vim.pending.clear();
        vim.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::NONE,
        )));
        assert_eq!(vim.ide_ui.directory_path, "");
        assert!(vim.pending.iter().any(|action| {
            matches!(action, PendingAction::LoadIdeDirectory { path: Some(path), .. } if path.is_empty())
        }));

        let mut arrows = app_with_entries();
        arrows.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(arrows.ide_ui.directory_path, "src");
        arrows.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::NONE,
        )));
        assert_eq!(arrows.ide_ui.directory_path, "");
    }

    #[test]
    fn snapshot_ide_at_three_sizes() {
        let mut app = app_with_entries();
        apply_hit(&mut app, IdeAction::SelectEntry(1));
        app.ide_ui
            .editor
            .set_text("fn main() {\n    println!(\"hello\");\n}\n");
        app.ide_ui.dirty = false;
        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            insta::assert_debug_snapshot!(
                format!("ide_{width}x{height}"),
                terminal.backend().buffer()
            );
        }
    }
}
