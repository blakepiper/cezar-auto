//! Notification plumbing: desktop notifications via `notify-rust`, gated on Settings →
//! Notifications, and a
//! terminal-title update via `OSC 0` (`crossterm::terminal::SetTitle`) that is always on —
//! the same way a favicon badge never asks permission. Both are best-effort: a missing
//! notification daemon, sound player, or terminal that ignores title escapes must never crash
//! or block the render loop.

use std::io::{Write, stdout};
#[cfg(unix)]
use std::process::{Command, Stdio};

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

/// Play the platform's task-complete sound, if notifications are enabled.
///
/// Terminal bells are commonly disabled or configured as visual-only. Use the platform's
/// event-sound player instead, reaping it on a short-lived background thread so playback never
/// blocks the render loop. If a player is unavailable, retain the terminal bell as a best-effort
/// fallback.
pub fn play_sound(enabled: bool) {
    if !enabled {
        return;
    }

    #[cfg(unix)]
    {
        let (program, args) = sound_command();
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return;
        }
    }

    let mut output = stdout();
    let _ = output.write_all(b"\x07");
    let _ = output.flush();
}

#[cfg(all(unix, not(target_os = "macos")))]
fn sound_command() -> (&'static str, &'static [&'static str]) {
    (
        "canberra-gtk-play",
        &["--id=complete", "--description=coducktor"],
    )
}

#[cfg(target_os = "macos")]
fn sound_command() -> (&'static str, &'static [&'static str]) {
    ("/usr/bin/afplay", &["/System/Library/Sounds/Glass.aiff"])
}

/// Ring the terminal bell.
///
/// Distinct from [`play_sound`], which targets the desktop: a bell is what marks a background
/// tmux window or terminal tab as having activity, so it is the only signal that reaches a user
/// whose run finished in a pane they are not looking at. Best-effort like everything else here.
pub fn bell() {
    let mut output = stdout();
    let _ = output.write_all(b"\x07");
    let _ = output.flush();
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

#[cfg(test)]
mod tests {
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn completion_sound_uses_the_freedesktop_event_player() {
        assert_eq!(
            super::sound_command(),
            (
                "canberra-gtk-play",
                ["--id=complete", "--description=coducktor"].as_slice()
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn completion_sound_uses_the_system_player() {
        assert_eq!(
            super::sound_command(),
            (
                "/usr/bin/afplay",
                ["/System/Library/Sounds/Glass.aiff"].as_slice()
            )
        );
    }
}
