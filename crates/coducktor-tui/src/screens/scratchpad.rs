//! A per-project quick-note editor persisted under the user's Coducktor home, outside Git.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, ConfirmRequest, PendingAction, Route};
use crate::clipboard::{self, ClipboardContent};
use crate::diff::Highlighter;
use crate::widgets::editor::Editor;

pub struct ScratchpadUi {
    pub project: String,
    pub editor: Editor,
    pub loaded: bool,
    pub saving: bool,
    pub viewport: usize,
    highlighter: Highlighter,
}

impl Default for ScratchpadUi {
    fn default() -> Self {
        Self {
            project: String::new(),
            editor: Editor::default(),
            loaded: false,
            saving: false,
            viewport: 0,
            highlighter: Highlighter::new(true),
        }
    }
}

pub fn open(app: &mut App, project: &str) {
    if app.scratchpad_ui.project != project {
        app.scratchpad_ui = ScratchpadUi {
            project: project.to_owned(),
            ..ScratchpadUi::default()
        };
    }
    app.navigate_route(Route::Scratchpad {
        project: project.to_owned(),
    });
    if !app.scratchpad_ui.loaded {
        app.pending.push(PendingAction::LoadScratchpad {
            project: project.to_owned(),
        });
    }
}

pub fn request_clear(app: &mut App) {
    if !matches!(app.route(), Route::Scratchpad { .. }) {
        app.notice = Some("open the scratchpad before clearing it".to_owned());
        return;
    }
    let project = app.scratchpad_ui.project.clone();
    app.confirm = Some(ConfirmRequest {
        text: "Clear this scratchpad? This cannot be undone.".to_owned(),
        action: PendingAction::ClearScratchpad { project },
    });
}

pub(crate) fn clear_after_confirmation(app: &mut App, project: &str) {
    if app.scratchpad_ui.project != project {
        return;
    }
    app.scratchpad_ui.editor.set_text("");
    app.scratchpad_ui.loaded = true;
    app.pending.retain(|action| {
        !matches!(action, PendingAction::LoadScratchpad { project: queued } if queued == project)
    });
    queue_save(app);
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = if !app.scratchpad_ui.loaded {
        "Scratchpad — loading…"
    } else if app.scratchpad_ui.saving {
        "Scratchpad — saving…"
    } else {
        "Scratchpad — saved locally"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(app.theme.palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.scratchpad_ui.viewport = inner.height as usize;
    app.scratchpad_ui
        .editor
        .ensure_caret_visible(app.scratchpad_ui.viewport);
    let lines = app.scratchpad_ui.editor.render_wrapped_lines(
        "scratchpad.md",
        &app.scratchpad_ui.highlighter,
        &app.theme,
        app.scratchpad_ui.viewport,
        inner.width,
        true,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('s') => {
                queue_save(app);
                return true;
            }
            KeyCode::Char('a') => {
                app.scratchpad_ui.editor.select_all();
                return true;
            }
            KeyCode::Char('c') => {
                copy_selection(app);
                return true;
            }
            KeyCode::Char('x') => {
                cut_selection(app);
                return true;
            }
            KeyCode::Char('v') => {
                paste_clipboard(app);
                return true;
            }
            KeyCode::Char('k') => {
                request_clear(app);
                return true;
            }
            _ => {}
        }
    }
    let editor = &mut app.scratchpad_ui.editor;
    let selecting = key.modifiers.contains(KeyModifiers::SHIFT);
    let changed = match key.code {
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            editor.insert_text(&character.to_string());
            true
        }
        KeyCode::Enter => {
            editor.insert_text("\n");
            true
        }
        KeyCode::Backspace => {
            if editor.has_selection() {
                editor.delete_selection()
            } else {
                editor.backspace();
                true
            }
        }
        KeyCode::Delete => {
            if editor.has_selection() {
                editor.delete_selection()
            } else {
                editor.delete_forward();
                true
            }
        }
        KeyCode::Left => {
            prepare_cursor_move(editor, selecting);
            editor.move_left();
            false
        }
        KeyCode::Right => {
            prepare_cursor_move(editor, selecting);
            editor.move_right();
            false
        }
        KeyCode::Up => {
            prepare_cursor_move(editor, selecting);
            editor.move_up();
            false
        }
        KeyCode::Down => {
            prepare_cursor_move(editor, selecting);
            editor.move_down();
            false
        }
        KeyCode::Home => {
            prepare_cursor_move(editor, selecting);
            editor.move_home();
            false
        }
        KeyCode::End => {
            prepare_cursor_move(editor, selecting);
            editor.move_end();
            false
        }
        _ => return false,
    };
    if changed {
        queue_save(app);
    }
    true
}

fn prepare_cursor_move(editor: &mut Editor, selecting: bool) {
    if selecting {
        editor.begin_selection();
    } else {
        editor.clear_selection();
    }
}

fn copy_selection(app: &mut App) {
    let Some(text) = app.scratchpad_ui.editor.selected_text() else {
        return;
    };
    if let Err(error) = clipboard::write_text(&text) {
        app.notice = Some(error);
    }
}

fn cut_selection(app: &mut App) {
    let Some(text) = app.scratchpad_ui.editor.selected_text() else {
        return;
    };
    if let Err(error) = clipboard::write_text(&text) {
        app.notice = Some(error);
        return;
    }
    if app.scratchpad_ui.editor.delete_selection() {
        queue_save(app);
    }
}

fn paste_clipboard(app: &mut App) {
    match clipboard::read() {
        Ok(ClipboardContent::Text(text)) => {
            app.scratchpad_ui.editor.insert_text(&text);
            queue_save(app);
        }
        Ok(ClipboardContent::ImagePng(_)) => {
            app.notice = Some("scratchpad clipboard paste supports text only".to_owned());
        }
        Err(error) => app.notice = Some(error),
    }
}

fn queue_save(app: &mut App) {
    let project = app.scratchpad_ui.project.clone();
    let content = app.scratchpad_ui.editor.text.clone();
    app.pending.retain(|action| {
        !matches!(action, PendingAction::SaveScratchpad { project: queued, .. } if queued == &project)
    });
    app.pending
        .push(PendingAction::SaveScratchpad { project, content });
    app.scratchpad_ui.saving = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::theme::Theme;

    #[test]
    fn typing_queues_a_project_local_save() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.pending.clear();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SaveScratchpad { project, content }
                if project == "main" && content == "x"
        )));
    }

    #[test]
    fn shift_arrows_select_and_backspace_deletes_the_selection() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.pending.clear();
        app.scratchpad_ui.editor.set_text("scratchpad");
        app.scratchpad_ui.editor.move_end();

        for code in [KeyCode::Left, KeyCode::Left, KeyCode::Left] {
            handle_key(&mut app, KeyEvent::new(code, KeyModifiers::SHIFT));
        }
        assert_eq!(
            app.scratchpad_ui.editor.selected_text().as_deref(),
            Some("pad")
        );

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(app.scratchpad_ui.editor.text, "scratch");
        assert!(!app.scratchpad_ui.editor.has_selection());
    }

    #[test]
    fn clear_scratchpad_requires_confirmation_and_queues_an_empty_save() {
        let mut app = App::new("main", Theme::detect(), Keymap::default());
        open(&mut app, "main");
        app.pending.clear();
        app.scratchpad_ui.editor.set_text("discard me");
        app.execute_command("clear-scratchpad");
        assert!(app.confirm.is_some());

        app.handle_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        )));
        assert!(app.confirm.is_none());
        assert!(app.scratchpad_ui.editor.text.is_empty());
        assert!(app.pending.iter().any(|action| matches!(
            action,
            PendingAction::SaveScratchpad { project, content }
                if project == "main" && content.is_empty()
        )));
    }
}
