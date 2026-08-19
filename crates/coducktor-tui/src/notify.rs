//! Notification plumbing: desktop notifications via `notify-rust`, gated on Settings →
//! Notifications, and a
//! terminal-title update via `OSC 0` (`crossterm::terminal::SetTitle`) that is always on —
//! the same way a favicon badge never asks permission. Both are best-effort: a missing
//! notification daemon or a terminal that ignores title escapes must never crash or block
//! the render loop.

use std::io::{Write, stdout};

use crossterm::execute;
use crossterm::terminal::SetTitle;

/// Update the terminal tab title.
pub fn set_title(title: &str) {
    let _ = execute!(stdout(), SetTitle(title));
}

/// Fire one desktop notification, if the user has turned them on.
pub fn notify(enabled: bool, summary: &str, body: &str) {
    if !enabled {
        return;
    }
    let _ = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("coducktor")
        .show();
}

/// Ring the user's terminal bell, if notifications are enabled.
///
/// This deliberately uses the terminal's native bell instead of adding an audio playback
/// dependency. Terminals may choose their own sound, visual bell, or silence it entirely.
pub fn bell(enabled: bool) {
    if enabled {
        let mut output = stdout();
        let _ = output.write_all(b"\x07");
        let _ = output.flush();
    }
}

/// The always-on title, reflecting how many of the current project's tasks need the user —
/// unconditional on the notification permission toggle.
pub fn title_for(needs_you: usize) -> String {
    if needs_you == 0 {
        "coducktor".to_owned()
    } else {
        format!("coducktor · {needs_you} needs you")
    }
}
