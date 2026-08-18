//! Durable, sanitized provider usage observations.
//!
//! The store is deliberately independent of runner protocol types. It accepts only the
//! provider-neutral contract shape, salvages valid entries from a damaged array, and never
//! replaces a corrupt source file while loading it.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use coducktor_contract::ProviderUsageSnapshot;
use coducktor_contract::compat::ExtraFields;

use super::config::atomic_write_json_sync;

const CURRENT_VERSION: u32 = 1;
const STORE_KEYS: &[&str] = &["version", "snapshots"];
const MAX_SNAPSHOTS: usize = 512;
const MAX_WINDOWS_PER_SNAPSHOT: usize = 32;
const MAX_TEXT_LENGTH: usize = 512;

/// The persisted usage cache. `extra` keeps fields written by a newer Coducktor build alive
/// when an older build performs a merge-write.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStore {
    pub version: u32,
    pub snapshots: Vec<ProviderUsageSnapshot>,
    pub extra: ExtraFields,
}

impl Default for ProviderUsageStore {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            snapshots: Vec::new(),
            extra: Map::new(),
        }
    }
}

/// The result of a best-effort load. A warning is returned once per call, leaving policy about
/// where to display it to the caller (startup notice, debug log, or headless command).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageLoad {
    pub store: ProviderUsageStore,
    pub warning: Option<String>,
}

/// Load a cache without ever making malformed state fatal to startup.
pub fn load_provider_usage(path: &Path) -> ProviderUsageLoad {
    let Ok(raw) = fs::read_to_string(path) else {
        return ProviderUsageLoad {
            store: ProviderUsageStore::default(),
            warning: None,
        };
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return warning("provider usage cache is not valid JSON");
    };
    let Some(object) = value.as_object() else {
        return warning("provider usage cache is not a JSON object");
    };

    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(CURRENT_VERSION);
    let Some(entries) = object.get("snapshots").and_then(Value::as_array) else {
        return ProviderUsageLoad {
            store: ProviderUsageStore {
                version,
                snapshots: Vec::new(),
                extra: extra_fields(object),
            },
            warning: object.contains_key("snapshots").then_some(
                "provider usage cache snapshots are not an array; starting with an empty cache"
                    .to_owned(),
            ),
        };
    };

    let mut snapshots = Vec::with_capacity(entries.len().min(MAX_SNAPSHOTS));
    let mut dropped = 0usize;
    for entry in entries.iter().take(MAX_SNAPSHOTS) {
        match serde_json::from_value::<ProviderUsageSnapshot>(entry.clone()) {
            Ok(mut snapshot) => {
                snapshot.stale = true;
                snapshots.push(sanitize_snapshot(snapshot));
            }
            Err(_) => dropped += 1,
        }
    }
    if entries.len() > MAX_SNAPSHOTS {
        dropped += entries.len() - MAX_SNAPSHOTS;
    }

    ProviderUsageLoad {
        store: ProviderUsageStore {
            version,
            snapshots,
            extra: extra_fields(object),
        },
        warning: (dropped > 0).then(|| {
            format!("provider usage cache ignored {dropped} malformed or over-limit snapshot(s)")
        }),
    }
}

/// Atomically persist a sanitized snapshot cache with the workspace file permissions.
pub fn save_provider_usage(path: &Path, store: &ProviderUsageStore) -> io::Result<()> {
    let value = store.to_value();
    atomic_write_json_sync(path, &value)
}

impl ProviderUsageStore {
    fn to_value(&self) -> Value {
        let snapshots = self
            .snapshots
            .iter()
            .take(MAX_SNAPSHOTS)
            .map(|snapshot| {
                serde_json::to_value(sanitize_snapshot(snapshot.clone()))
                    .unwrap_or_else(|_| Value::Object(Map::new()))
            })
            .collect::<Vec<_>>();
        let mut value = self.extra.clone();
        value.insert(
            "version".to_owned(),
            Value::from(self.version.max(CURRENT_VERSION)),
        );
        value.insert("snapshots".to_owned(), Value::Array(snapshots));
        Value::Object(value)
    }
}

fn warning(message: &str) -> ProviderUsageLoad {
    ProviderUsageLoad {
        store: ProviderUsageStore::default(),
        warning: Some(message.to_owned()),
    }
}

fn extra_fields(object: &Map<String, Value>) -> ExtraFields {
    object
        .iter()
        .filter(|(key, _)| !STORE_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn bounded_text(value: String) -> String {
    value.chars().take(MAX_TEXT_LENGTH).collect()
}

fn sanitize_snapshot(mut snapshot: ProviderUsageSnapshot) -> ProviderUsageSnapshot {
    snapshot.profile_id = bounded_text(snapshot.profile_id);
    snapshot.upstream_provider = snapshot.upstream_provider.map(bounded_text);
    snapshot.fetched_at = bounded_text(snapshot.fetched_at);
    snapshot.source = bounded_text(snapshot.source);
    snapshot.windows.truncate(MAX_WINDOWS_PER_SNAPSHOT);
    for window in &mut snapshot.windows {
        window.id = window.id.take().map(bounded_text);
        window.resets_at = window.resets_at.take().map(bounded_text);
        if let Some(used) = window.used_percent {
            window.used_percent = Some(used.clamp(0.0, 100.0));
        }
    }
    if let Some(error) = &mut snapshot.error {
        error.code = bounded_text(std::mem::take(&mut error.code));
        error.message = bounded_text(std::mem::take(&mut error.message));
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::{
        ProviderUsageHealth, ProviderUsageWindow, ProviderUsageWindowKind, QuotaProvider,
        UsageConfidence,
    };
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn snapshot(profile_id: &str) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider: QuotaProvider::OpenCode,
            profile_id: profile_id.to_owned(),
            upstream_provider: Some("anthropic".to_owned()),
            health: ProviderUsageHealth::Unknown,
            confidence: Some(UsageConfidence::Unknown),
            fetched_at: "2026-08-18T00:00:00Z".to_owned(),
            source: "test".to_owned(),
            stale: false,
            windows: vec![ProviderUsageWindow {
                id: Some("weekly".to_owned()),
                kind: ProviderUsageWindowKind::Long,
                used_percent: Some(150.0),
                resets_at: None,
                hard_limit_reached: None,
            }],
            consumption: None,
            error: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn missing_cache_is_an_empty_non_warning() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_provider_usage(&dir.path().join("provider-usage.json"));
        assert!(loaded.store.snapshots.is_empty());
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn malformed_entries_are_salvaged_and_restored_entries_are_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-usage.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "future": true,
                "snapshots": [snapshot("default"), {"provider": "bad"}],
            }))
            .unwrap(),
        )
        .unwrap();
        let loaded = load_provider_usage(&path);
        assert_eq!(loaded.store.snapshots.len(), 1);
        assert!(loaded.store.snapshots[0].stale);
        assert!(loaded.store.extra.contains_key("future"));
        assert!(loaded.warning.is_some());
    }

    #[test]
    fn save_is_atomic_private_and_preserves_unknown_top_level_and_entry_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-usage.json");
        let mut store = ProviderUsageStore::default();
        store
            .extra
            .insert("future".to_owned(), json!({"yes": true}));
        let mut entry = snapshot("default");
        entry.extra.insert("futureEntry".to_owned(), json!(42));
        store.snapshots.push(entry);
        save_provider_usage(&path, &store).unwrap();
        let loaded = load_provider_usage(&path);
        assert_eq!(loaded.store.extra["future"], json!({"yes": true}));
        assert_eq!(loaded.store.snapshots[0].extra["futureEntry"], json!(42));
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            loaded.store.snapshots[0].windows[0].used_percent,
            Some(100.0)
        );
    }
}
