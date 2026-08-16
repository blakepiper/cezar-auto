//! Notification plumbing (spec §9.2's "Browser tab title / favicon badge" parity row):
//! desktop notifications via `notify-rust`, gated on Settings → Notifications, and a
//! terminal-title update via `OSC 0` (`crossterm::terminal::SetTitle`) that is always on —
//! the same way a favicon badge never asks permission. Both are best-effort: a missing
//! notification daemon or a terminal that ignores title escapes must never crash or block
//! the render loop.

use std::io::stdout;

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

/// The always-on title, reflecting how many of the current project's tasks need the user —
/// spec §9.2's favicon-badge parity, unconditional on the notification permission toggle.
pub fn title_for(needs_you: usize) -> String {
    if needs_you == 0 {
        "coducktor".to_owned()
    } else {
        format!("coducktor · {needs_you} needs you")
    }
}
