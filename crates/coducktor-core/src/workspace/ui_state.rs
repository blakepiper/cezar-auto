//! `~/.coducktor/ui-state.json` — global GUI state, the workspace twin of the per-repo
//! `.ai/coducktor/ui-state.json`. Mirrors `packages/coducktor/src/workspace/ui-state.ts`.
//! Cross-project prefs (appearance, notifications, curated skills) live here; per-project
//! prefs stay in each repo's own file, and prefs that describe the BROWSER rather than
//! the workspace stay in that browser's `localStorage`.

use std::fs;
use std::io;
use std::path::Path;

use coducktor_contract::WorkspaceUiState;

use super::config::atomic_write_json_sync;

/// Read `~/.coducktor/ui-state.json` on demand — never cached, never throws. Missing,
/// unreadable, malformed, or non-object all degrade to the empty bag. Deliberately NOT
/// schema-validated on read (mirrors the TS source's own comment): a single malformed
/// pref must not discard the whole bag, and `WorkspaceUiState`'s fields are already all
/// optional, so a partially-unparseable object still yields whatever fields DID parse.
pub fn read_workspace_ui_state(path: &Path) -> WorkspaceUiState {
    let Ok(raw) = fs::read_to_string(path) else {
        return WorkspaceUiState::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Read-modify-write for `~/.coducktor/ui-state.json`, written through the same atomic
/// per-writer tmp+rename `0600` pattern as `workspace::config`'s writer. Mirrors
/// `workspace/ui-state.ts::mergeWriteWorkspaceUiState`.
///
/// `path` is resolved once by the caller for the same reason
/// `workspace::config::merge_write_workspace_config` documents.
pub fn merge_write_workspace_ui_state(
    path: &Path,
    mutator: impl FnOnce(&mut WorkspaceUiState),
) -> io::Result<WorkspaceUiState> {
    let mut next = read_workspace_ui_state(path);
    mutator(&mut next);
    let value = serde_json::to_value(&next).expect("WorkspaceUiState always serializes");
    atomic_write_json_sync(path, &value)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_empty_bag() {
        let path = Path::new("/does/not/exist/ui-state.json");
        assert_eq!(read_workspace_ui_state(path), WorkspaceUiState::default());
    }

    #[test]
    fn a_malformed_file_degrades_to_the_empty_bag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui-state.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(read_workspace_ui_state(&path), WorkspaceUiState::default());
    }

    #[test]
    fn a_json_array_degrades_to_the_empty_bag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui-state.json");
        fs::write(&path, "[1,2,3]").unwrap();
        assert_eq!(read_workspace_ui_state(&path), WorkspaceUiState::default());
    }

    #[test]
    fn merge_write_reads_back_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui-state.json");
        merge_write_workspace_ui_state(&path, |state| {
            state.notifications = Some(coducktor_contract::workspace::NotificationsUiState {
                enabled: Some(false),
                extra: Default::default(),
            });
        })
        .unwrap();
        let reloaded = read_workspace_ui_state(&path);
        assert_eq!(reloaded.notifications.unwrap().enabled, Some(false));
    }
}
