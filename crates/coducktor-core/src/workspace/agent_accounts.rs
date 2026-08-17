//! `~/.coducktor/agent-accounts.json` — extra config dirs for a second login of the same
//! agent CLI, plus which one each project uses.
//!
//! Its OWN file rather than a key in `config.json`, and that is the whole point: a
//! coducktor build that has never heard of accounts does not open this file, so it
//! cannot drop them — a guarantee `.passthrough()` in `workspace::config` cannot make on
//! behalf of a build a user switches to, and one that evaporates the moment any build
//! fails to parse that file and degrades to defaults (the next merge-write then rewrites
//! it without them). Same house rules as `workspace::config`: every field
//! optional/defaulted with a per-key catch, `.passthrough()` at every object level,
//! per-entry salvage for `accounts`, atomic tmp+rename at `0600`.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use coducktor_contract::{DEFAULT_AGENT_ACCOUNT_ID, Runner};

use super::config::{atomic_write_json_sync, is_valid_slug};
use crate::zod;

/// `id` slug rule — the project rule, for the same URL/segment-safety reason.
pub use super::config::is_valid_slug as is_valid_account_id;

/// C0 controls + DEL. A path containing one is never legitimate and would be interpolated into a
/// shell command by the CLI handoff.
pub fn has_control_chars(value: &str) -> bool {
    value
        .chars()
        .any(|c| (c as u32) <= 0x1f || c as u32 == 0x7f)
}

/// Providers whose credentials follow their config dir, so more than one login is even
/// representable. Claude uses `CLAUDE_CONFIG_DIR`, Codex uses `CODEX_HOME`, and OpenCode/Pi do
/// not support multiple config directories.
pub fn supports_profiles(provider: Runner) -> bool {
    matches!(provider, Runner::Claude | Runner::Codex)
}

/// One extra config dir for a provider. `id`, `provider`, and `config_dir`
/// are load-bearing and carry no catch: a row missing any of them names no account and
/// is dropped by the per-entry salvage. `config_dir` is stored AS WRITTEN — a literal `~`
/// survives — every consumer expands it through `paths::expand_tilde`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentAccount {
    pub id: String,
    pub provider: Runner,
    pub config_dir: String,
    pub label: String,
    pub added_at: String,
    pub extra: Map<String, Value>,
}

const AGENT_ACCOUNT_KEYS: &[&str] = &["id", "provider", "configDir", "label", "addedAt"];

impl AgentAccount {
    /// `None` when `id`, `provider`, or `configDir` fails validation.
    fn parse(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = zod::regex_str(object.get("id"), |s| {
            is_valid_slug(s) && s != DEFAULT_AGENT_ACCOUNT_ID
        })?
        .to_owned();
        let provider: Runner = object
            .get("provider")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .filter(|p| supports_profiles(*p))?;
        let config_dir = object
            .get("configDir")
            .and_then(Value::as_str)
            .filter(|s| {
                let len = s.chars().count();
                (1..=4096).contains(&len) && !has_control_chars(s)
            })?
            .to_owned();
        Some(Self {
            id,
            provider,
            config_dir,
            label: zod::capped_str_or(object.get("label"), 200, ""),
            added_at: zod::capped_str_or(object.get("addedAt"), 64, ""),
            extra: zod::extra_fields(object, AGENT_ACCOUNT_KEYS),
        })
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                ("id", Value::String(self.id.clone())),
                (
                    "provider",
                    serde_json::to_value(self.provider).expect("Runner always serializes"),
                ),
                ("configDir", Value::String(self.config_dir.clone())),
                ("label", Value::String(self.label.clone())),
                ("addedAt", Value::String(self.added_at.clone())),
            ],
        )
    }
}

/// One project's per-provider account choice. An absent key means the discovered
/// default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentAccountSelection {
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub opencode: Option<String>,
    pub pi: Option<String>,
    pub extra: Map<String, Value>,
}

const SELECTION_KEYS: &[&str] = &["claude", "codex", "opencode", "pi"];

impl AgentAccountSelection {
    fn parse(value: Option<&Value>) -> Self {
        let object = zod::as_map(value);
        Self {
            claude: zod::capped_str_opt(zod::field(object, "claude"), 64),
            codex: zod::capped_str_opt(zod::field(object, "codex"), 64),
            opencode: zod::capped_str_opt(zod::field(object, "opencode"), 64),
            pi: zod::capped_str_opt(zod::field(object, "pi"), 64),
            extra: object
                .map(|o| zod::extra_fields(o, SELECTION_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        zod::merge_extra(
            &self.extra,
            vec![
                (
                    "claude",
                    self.claude.clone().map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "codex",
                    self.codex.clone().map(Value::from).unwrap_or(Value::Null),
                ),
                (
                    "opencode",
                    self.opencode
                        .clone()
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                ),
                (
                    "pi",
                    self.pi.clone().map(Value::from).unwrap_or(Value::Null),
                ),
            ],
        )
    }

    /// This provider's choice, if any.
    pub fn get(&self, provider: Runner) -> Option<&str> {
        match provider {
            Runner::Claude => self.claude.as_deref(),
            Runner::Codex => self.codex.as_deref(),
            Runner::OpenCode => self.opencode.as_deref(),
            Runner::Pi => self.pi.as_deref(),
        }
    }
}

const STORE_KEYS: &[&str] = &["version", "accounts", "defaults", "selections"];

/// The durable agent-account store in `~/.coducktor/agent-accounts.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentAccountStore {
    /// Format cursor for THIS file, independent of `workspace::config`'s
    /// `schema_version`. Nothing reads it yet.
    pub version: u32,
    /// Per-entry salvage, first-wins on a duplicated id.
    pub accounts: Vec<AgentAccount>,
    /// The per-provider account a repo with no selection of its own uses.
    pub defaults: AgentAccountSelection,
    /// Repo root → per-provider choice, keyed by the REALPATH'D root. Per-entry salvage.
    pub selections: Vec<(String, AgentAccountSelection)>,
    pub extra: Map<String, Value>,
}

impl Default for AgentAccountStore {
    fn default() -> Self {
        Self::parse(&Value::Object(Default::default()))
    }
}

impl AgentAccountStore {
    fn parse(raw: &Value) -> Self {
        let object = raw.as_object();
        let accounts = zod::field(object, "accounts")
            .and_then(Value::as_array)
            .map(|entries| {
                let mut seen = Vec::new();
                entries
                    .iter()
                    .filter_map(AgentAccount::parse)
                    .filter(|account| {
                        if seen.contains(&account.id) {
                            false
                        } else {
                            seen.push(account.id.clone());
                            true
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let selections = zod::field(object, "selections")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(root, value)| (root.clone(), AgentAccountSelection::parse(Some(value))))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            version: zod::bounded_i64(zod::field(object, "version"), 0, i64::MAX, 1) as u32,
            accounts,
            defaults: AgentAccountSelection::parse(zod::field(object, "defaults")),
            selections,
            extra: object
                .map(|o| zod::extra_fields(o, STORE_KEYS))
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> Value {
        let selections: Map<String, Value> = self
            .selections
            .iter()
            .map(|(root, selection)| (root.clone(), selection.to_value()))
            .collect();
        zod::merge_extra(
            &self.extra,
            vec![
                ("version", Value::from(self.version)),
                (
                    "accounts",
                    Value::from(
                        self.accounts
                            .iter()
                            .map(AgentAccount::to_value)
                            .collect::<Vec<_>>(),
                    ),
                ),
                ("defaults", self.defaults.to_value()),
                ("selections", Value::Object(selections)),
            ],
        )
    }

    /// The selection stored for `repo_root`, matched on the literal spelling only —
    /// realpath normalization is the caller's job. The two-step lookup is split here because
    /// realpath resolution needs filesystem access this pure function doesn't take).
    pub fn selection_for_root(&self, repo_root: &str) -> Option<&AgentAccountSelection> {
        self.selections
            .iter()
            .find(|(root, _)| root == repo_root)
            .map(|(_, selection)| selection)
    }

    /// The account `provider` uses for `repo_root` (already realpath-normalized by the
    /// caller), or `None` when nothing has an opinion. Repo first, then the machine-wide
    /// default — a repo that chose is never overruled by a later change to the default.
    pub fn selection_for(&self, repo_root: Option<&str>, provider: Runner) -> Option<&str> {
        repo_root
            .and_then(|root| self.selection_for_root(root))
            .and_then(|selection| selection.get(provider))
            .or_else(|| self.defaults.get(provider))
    }
}

/// Read the store on demand — never cached, never throws. A missing file returns the
/// empty default. A corrupt file degrades to the default, left on disk untouched.
pub fn load_agent_accounts(path: &Path) -> AgentAccountStore {
    let Ok(raw) = fs::read_to_string(path) else {
        return AgentAccountStore::default();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return AgentAccountStore::default();
    };
    AgentAccountStore::parse(&parsed)
}

/// Read-modify-write merge: re-read, apply `mutator`, and atomically rename the write.
pub fn merge_write_agent_accounts(
    path: &Path,
    mutator: impl FnOnce(&mut AgentAccountStore),
) -> io::Result<AgentAccountStore> {
    let mut next = load_agent_accounts(path);
    mutator(&mut next);
    atomic_write_json_sync(path, &next.to_value())?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_id_is_refused() {
        let raw = serde_json::json!({ "id": "default", "provider": "claude", "configDir": "~/.claude-2" });
        assert!(AgentAccount::parse(&raw).is_none());
    }

    #[test]
    fn a_provider_without_profile_support_is_refused() {
        let raw =
            serde_json::json!({ "id": "second", "provider": "opencode", "configDir": "~/.oc-2" });
        assert!(AgentAccount::parse(&raw).is_none());
    }

    #[test]
    fn a_control_character_in_config_dir_is_refused() {
        let raw = serde_json::json!({ "id": "second", "provider": "claude", "configDir": "~/.claude\u{0007}" });
        assert!(AgentAccount::parse(&raw).is_none());
    }

    #[test]
    fn duplicate_ids_are_first_wins() {
        let raw = serde_json::json!({
            "accounts": [
                { "id": "second", "provider": "claude", "configDir": "~/first-write" },
                { "id": "second", "provider": "claude", "configDir": "~/second-write" },
            ],
        });
        let store = AgentAccountStore::parse(&raw);
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].config_dir, "~/first-write");
    }

    #[test]
    fn selection_prefers_the_repo_over_the_machine_default() {
        let raw = serde_json::json!({
            "defaults": { "claude": "machine-default" },
            "selections": { "/repo/shop": { "claude": "repo-choice" } },
        });
        let store = AgentAccountStore::parse(&raw);
        assert_eq!(
            store.selection_for(Some("/repo/shop"), Runner::Claude),
            Some("repo-choice")
        );
        assert_eq!(
            store.selection_for(Some("/repo/other"), Runner::Claude),
            Some("machine-default")
        );
        assert_eq!(
            store.selection_for(None, Runner::Claude),
            Some("machine-default")
        );
    }

    #[test]
    fn round_trip_through_parse_and_serialize_is_stable() {
        let raw = serde_json::json!({
            "accounts": [{ "id": "second", "provider": "codex", "configDir": "~/.codex-2", "label": "Work" }],
            "selections": { "/repo/shop": { "codex": "second" } },
        });
        let once = AgentAccountStore::parse(&raw);
        let twice = AgentAccountStore::parse(&once.to_value());
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_write_reads_back_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-accounts.json");
        merge_write_agent_accounts(&path, |store| {
            store.accounts.push(AgentAccount {
                id: "second".to_owned(),
                provider: Runner::Claude,
                config_dir: "~/.claude-2".to_owned(),
                label: String::new(),
                added_at: String::new(),
                extra: Map::new(),
            });
        })
        .unwrap();
        let reloaded = load_agent_accounts(&path);
        assert_eq!(reloaded.accounts.len(), 1);
    }
}
