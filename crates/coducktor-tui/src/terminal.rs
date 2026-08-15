use std::io::{self, Write};
use std::panic;

use crossterm::cursor;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type AppTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub fn install_panic_hook() {
    let previous_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore();
        previous_hook(panic_info);
    }));
}

pub fn setup() -> io::Result<AppTerminal> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    ) {
        let _ = disable_raw_mode();
        return Err(error);
    }

    let terminal = Terminal::new(CrosstermBackend::new(stdout));
    if terminal.is_err() {
        let _ = restore();
    }
    terminal
}

pub fn restore() -> io::Result<()> {
    let raw_mode_result = disable_raw_mode();

    let mut stdout = io::stdout();
    let screen_result = execute!(
        stdout,
        LeaveAlternateScreen,
        DisableMouseCapture,
        cursor::Show
    );
    let flush_result = stdout.flush();

    raw_mode_result.and(screen_result).and(flush_result)
}
