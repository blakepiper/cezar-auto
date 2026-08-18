//! Embedded per-project PTY sessions.
//!
//! The Terminal tab runs a real shell inside the cockpit instead of delegating to an
//! external terminal emulator. Each session owns a portable-pty master pair (the same
//! library WezTerm uses), a background reader thread feeding a `vt100::Parser`, and the
//! child process. The parser's cell grid is all the UI needs to render; the master is
//! kept for resize signals and the child is killed when the session is dropped.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::Parser;

/// Rows of scrollback kept per session. `vt100` stores whole rows, so this is cheap.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A running (or dead) shell inside the Terminal tab.
pub struct TerminalSession {
    parser: Arc<Mutex<Parser>>,
    exited: Arc<AtomicBool>,
    cwd: String,
    live: Option<Live>,
    size: (u16, u16),
}

/// The subprocess half of a session, absent in tests where only the parser grid exists.
struct Live {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl TerminalSession {
    /// Spawn `$SHELL` (falling back to `sh`) in `cwd` and start the reader thread.
    pub fn spawn(cwd: &Path, rows: u16, cols: u16) -> std::io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let (master, slave) = (pair.master, pair.slave);
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = slave
            .spawn_command(command)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let reader = master
            .try_clone_reader()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let writer = master
            .take_writer()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let parser = Arc::new(Mutex::new(Parser::new(rows, cols, SCROLLBACK_LINES)));
        let exited = Arc::new(AtomicBool::new(false));
        spawn_reader(reader, parser.clone(), exited.clone());
        Ok(Self {
            parser,
            exited,
            cwd: cwd.display().to_string(),
            live: Some(Live {
                master,
                writer,
                child,
            }),
            size: (rows, cols),
        })
    }

    /// A parser-only session for tests: no PTY, no child, no reader thread.
    #[cfg(test)]
    pub(crate) fn headless(rows: u16, cols: u16, cwd: &str) -> Self {
        Self {
            parser: Arc::new(Mutex::new(Parser::new(rows, cols, SCROLLBACK_LINES))),
            exited: Arc::new(AtomicBool::new(false)),
            cwd: cwd.to_owned(),
            live: None,
            size: (rows, cols),
        }
    }

    /// Feed bytes into the parser (tests only).
    #[cfg(test)]
    pub(crate) fn feed(&self, bytes: &[u8]) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.process(bytes);
        }
    }

    pub fn parser(&self) -> &Arc<Mutex<Parser>> {
        &self.parser
    }

    /// Whether the shell has exited (reader thread hit EOF).
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The current scrollback offset applied to the parser (`0` = live view).
    pub fn scrollback(&self) -> usize {
        if let Ok(parser) = self.parser.lock() {
            return parser.screen().scrollback();
        }
        0
    }

    /// Apply a scrollback offset (`0` = bottom). The parser clamps to what it holds.
    pub fn set_scrollback(&self, rows: usize) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.set_scrollback(rows);
        }
    }

    /// Forward key bytes into the shell.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(live) = self.live.as_mut() {
            live.writer.write_all(bytes)?;
            live.writer.flush()?;
        }
        Ok(())
    }

    /// Resize the parser grid and tell the kernel so the shell sees SIGWINCH. No-op
    /// while the requested size matches the last applied size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut parser) = self.parser.lock() {
            parser.set_size(rows, cols);
        }
        if let Some(live) = &self.live {
            let _ = live.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Kill the shell and its foreground job. Dropping the master afterwards lets the
    /// kernel SIGHUP anything left in the session, matching a closed terminal window.
    pub fn kill(&mut self) {
        if let Some(live) = &mut self.live {
            let _ = live.child.kill();
            let _ = live.child.wait();
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Read the PTY master until EOF, feeding the parser and flagging exit when done.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<Parser>>,
    exited: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut parser) = parser.lock() {
                        parser.process(&buffer[..n]);
                    }
                }
                Err(_) => break,
            }
        }
        exited.store(true, Ordering::Relaxed);
    });
}

/// Encode a crossterm key press as the byte sequence a terminal would send, so the
/// embedded shell sees an ordinary terminal input stream (readline, vim, `less` all
/// rely on these spellings). Returns `None` for keys with no standard spelling.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = modifier_code(alt, ctrl, shift);
    let mut bytes = Vec::new();
    match key.code {
        KeyCode::Char(character) => {
            if ctrl && character.is_ascii_lowercase() {
                bytes.push(character as u8 & 0x1f);
            } else if ctrl && character.is_ascii_uppercase() {
                bytes.push(character.to_ascii_lowercase() as u8 & 0x1f);
            } else if ctrl && matches!(character, ' ' | '@') {
                bytes.push(0);
            } else if ctrl && character.is_ascii() {
                bytes.push(character as u8 & 0x1f);
            } else {
                if alt {
                    bytes.push(0x1b);
                }
                let mut encoded = [0u8; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Tab => {
            if shift {
                bytes.extend_from_slice(b"\x1b[Z");
            } else {
                bytes.push(b'\t');
            }
        }
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Left => csi_cursor(&mut bytes, modifier, b'D'),
        KeyCode::Right => csi_cursor(&mut bytes, modifier, b'C'),
        KeyCode::Up => csi_cursor(&mut bytes, modifier, b'A'),
        KeyCode::Down => csi_cursor(&mut bytes, modifier, b'B'),
        KeyCode::Home => csi_cursor(&mut bytes, modifier, b'H'),
        KeyCode::End => csi_cursor(&mut bytes, modifier, b'F'),
        KeyCode::PageUp => csi_tilde(&mut bytes, modifier, 5),
        KeyCode::PageDown => csi_tilde(&mut bytes, modifier, 6),
        KeyCode::Insert => csi_tilde(&mut bytes, modifier, 2),
        KeyCode::Delete => csi_tilde(&mut bytes, modifier, 3),
        KeyCode::F(1) => bytes.extend_from_slice(b"\x1bOP"),
        KeyCode::F(2) => bytes.extend_from_slice(b"\x1bOQ"),
        KeyCode::F(3) => bytes.extend_from_slice(b"\x1bOR"),
        KeyCode::F(4) => bytes.extend_from_slice(b"\x1bOS"),
        KeyCode::F(number) if (5..=12).contains(&number) => {
            let tilde = match number {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            csi_tilde(&mut bytes, modifier, tilde);
        }
        _ => return None,
    }
    Some(bytes)
}

/// The xterm modifier code: 1 plain, 2 shift, 3 alt, 4 shift+alt, 5 ctrl, 6 shift+ctrl,
/// 7 alt+ctrl, 8 shift+alt+ctrl.
fn modifier_code(alt: bool, ctrl: bool, shift: bool) -> u8 {
    1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl)
}

fn csi_cursor(bytes: &mut Vec<u8>, modifier: u8, final_byte: u8) {
    if modifier == 1 {
        bytes.extend_from_slice(&[0x1b, b'[', final_byte]);
    } else {
        bytes.extend_from_slice(format!("\x1b[1;{modifier}{}", final_byte as char).as_bytes());
    }
}

fn csi_tilde(bytes: &mut Vec<u8>, modifier: u8, code: u8) {
    if modifier == 1 {
        bytes.extend_from_slice(format!("\x1b[{code}~").as_bytes());
    } else {
        bytes.extend_from_slice(format!("\x1b[{code};{modifier}~").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_characters_are_utf8() {
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(b"x".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('é'), KeyModifiers::NONE)),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn control_characters_become_control_bytes() {
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(vec![0x00])
        );
    }

    #[test]
    fn alt_precedes_a_character_with_escape() {
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn navigation_keys_use_csi_spellings() {
        assert_eq!(
            encode_key(key(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Delete, KeyModifiers::NONE)),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Home, KeyModifiers::ALT)),
            Some(b"\x1b[1;3H".to_vec())
        );
    }

    #[test]
    fn function_keys_use_standard_sequences() {
        assert_eq!(
            encode_key(key(KeyCode::F(1), KeyModifiers::NONE)),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(5), KeyModifiers::NONE)),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(12), KeyModifiers::NONE)),
            Some(b"\x1b[24~".to_vec())
        );
    }

    #[test]
    fn enter_tab_and_backspace_match_a_real_terminal() {
        assert_eq!(
            encode_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn a_headless_session_parses_output_and_tracks_exit() {
        let session = TerminalSession::headless(24, 80, "/tmp");
        assert!(!session.exited());
        session.feed(b"\x1b[31mred\x1b[0m\r\n");
        let parser = session.parser();
        let screen = {
            let parser = parser.lock().unwrap();
            parser.screen().contents_between(0, 0, 0, 3)
        };
        assert_eq!(screen, "red");
    }

    #[test]
    fn scrollback_applies_to_the_visible_grid() {
        let session = TerminalSession::headless(3, 80, "/tmp");
        session.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");
        let first_row = {
            let parser = session.parser().lock().unwrap();
            parser.screen().contents_between(0, 0, 0, 79)
        };
        assert_eq!(first_row, "five");
        session.set_scrollback(2);
        let first_row = {
            let parser = session.parser().lock().unwrap();
            parser.screen().contents_between(0, 0, 0, 79)
        };
        assert_eq!(first_row, "three");
    }
}
