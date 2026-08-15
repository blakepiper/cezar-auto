use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

const DEFAULT_KEYMAP: &str = include_str!("../../default-keymap.toml");

/// Input mode used to select a keymap table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyMode {
    Normal,
    Command,
}

/// Actions understood by the A3 shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    Quit,
    Tasks,
    GlobalTasks,
    Command,
    ExecuteCommand,
    Normal,
    Back,
    Forward,
    Noop,
}

impl ActionId {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "quit" => Some(Self::Quit),
            "tasks" => Some(Self::Tasks),
            "global-tasks" => Some(Self::GlobalTasks),
            "command" => Some(Self::Command),
            "execute-command" => Some(Self::ExecuteCommand),
            "normal" => Some(Self::Normal),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            "noop" => Some(Self::Noop),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawKeymap {
    #[serde(default)]
    normal: BTreeMap<String, String>,
    #[serde(default)]
    command: BTreeMap<String, String>,
}

/// The effective keymap after the embedded defaults and optional user override are merged.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: BTreeMap<KeyMode, BTreeMap<String, ActionId>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_toml(DEFAULT_KEYMAP).unwrap_or_else(|_| Self {
            bindings: BTreeMap::new(),
        })
    }
}

impl Keymap {
    pub fn load(user_path: Option<&Path>) -> io::Result<Self> {
        let mut keymap = Self::from_toml(DEFAULT_KEYMAP)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(path) = user_path
            && path.is_file()
        {
            let contents = fs::read_to_string(path)?;
            keymap.merge(Self::from_toml(&contents).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {error}", path.display()),
                )
            })?);
        }
        Ok(keymap)
    }

    pub fn from_toml(contents: &str) -> Result<Self, String> {
        let raw: RawKeymap = toml::from_str(contents).map_err(|error| error.to_string())?;
        let mut bindings = BTreeMap::new();
        bindings.insert(KeyMode::Normal, parse_bindings(raw.normal)?);
        bindings.insert(KeyMode::Command, parse_bindings(raw.command)?);
        Ok(Self { bindings })
    }

    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("DUCK_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".coducktor")))
            .map(|home| home.join("keymap.toml"))
    }

    pub fn action_for(&self, mode: KeyMode, event: &KeyEvent) -> Option<ActionId> {
        let key = key_id(event);
        self.bindings.get(&mode)?.get(&key).copied()
    }

    fn merge(&mut self, overlay: Self) {
        for (mode, bindings) in overlay.bindings {
            self.bindings.entry(mode).or_default().extend(bindings);
        }
    }
}

fn parse_bindings(raw: BTreeMap<String, String>) -> Result<BTreeMap<String, ActionId>, String> {
    raw.into_iter()
        .map(|(key, action)| {
            ActionId::parse(&action)
                .map(|action| (key, action))
                .ok_or_else(|| format!("unknown action {action:?}"))
        })
        .collect()
}

pub fn key_id(event: &KeyEvent) -> String {
    let prefix = if event.modifiers.contains(KeyModifiers::CONTROL) {
        "ctrl-"
    } else if event.modifiers.contains(KeyModifiers::ALT) {
        "alt-"
    } else {
        ""
    };
    let key = match event.code {
        KeyCode::Char(character) => character.to_ascii_lowercase().to_string(),
        KeyCode::Enter => "enter".to_owned(),
        KeyCode::Esc => "escape".to_owned(),
        KeyCode::Backspace => "backspace".to_owned(),
        KeyCode::Tab => "tab".to_owned(),
        KeyCode::BackTab => "backtab".to_owned(),
        KeyCode::Up => "up".to_owned(),
        KeyCode::Down => "down".to_owned(),
        KeyCode::Left => "left".to_owned(),
        KeyCode::Right => "right".to_owned(),
        KeyCode::F(number) => format!("f{number}"),
        _ => format!("{:?}", event.code).to_ascii_lowercase(),
    };
    format!("{prefix}{key}")
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn merges_user_bindings_over_defaults() {
        let mut keymap = Keymap::from_toml(DEFAULT_KEYMAP).unwrap();
        let overlay = Keymap::from_toml("[normal]\nq = \"tasks\"\n").unwrap();
        keymap.merge(overlay);
        let event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);

        assert_eq!(
            keymap.action_for(KeyMode::Normal, &event),
            Some(ActionId::Tasks)
        );
    }

    #[test]
    fn key_ids_include_control_modifiers() {
        let event = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(key_id(&event), "ctrl-o");
    }
}
