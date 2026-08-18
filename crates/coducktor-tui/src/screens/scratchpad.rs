//! A per-project quick-note editor persisted under the user's Coducktor home, outside Git.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, PendingAction, Route};
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
    let lines = app.scratchpad_ui.editor.render_lines(
        "scratchpad.md",
        &app.scratchpad_ui.highlighter,
        &app.theme,
        app.scratchpad_ui.viewport,
        true,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        queue_save(app);
        return true;
    }
    let editor = &mut app.scratchpad_ui.editor;
    let changed = match key.code {
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            editor.insert_char(character);
            true
        }
        KeyCode::Enter => {
            editor.insert_newline();
            true
        }
        KeyCode::Backspace => {
            editor.backspace();
            true
        }
        KeyCode::Delete => {
            editor.delete_forward();
            true
        }
        KeyCode::Left => {
            editor.move_left();
            false
        }
        KeyCode::Right => {
            editor.move_right();
            false
        }
        KeyCode::Up => {
            editor.move_up();
            false
        }
        KeyCode::Down => {
            editor.move_down();
            false
        }
        KeyCode::Home => {
            editor.move_home();
            false
        }
        KeyCode::End => {
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
}
