mod terminal;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::Alignment;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::terminal::AppTerminal;

fn main() -> io::Result<()> {
    terminal::install_panic_hook();
    let mut terminal = terminal::setup()?;
    let run_result = run(&mut terminal);
    let restore_result = terminal::restore();

    run_result.and(restore_result)
}

fn run(terminal: &mut AppTerminal) -> io::Result<()> {
    loop {
        terminal.draw(|frame| {
            let paragraph = Paragraph::new("Press q to quit")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("coducktor"));
            frame.render_widget(paragraph, frame.area());
        })?;

        if event::poll(Duration::from_millis(250))? && should_quit(&event::read()?) {
            break;
        }
    }

    Ok(())
}

fn should_quit(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('q')
                && key.modifiers.is_empty()
    )
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn plain_q_press_quits() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

        assert!(should_quit(&event));
    }

    #[test]
    fn modified_q_does_not_quit() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

        assert!(!should_quit(&event));
    }
}
