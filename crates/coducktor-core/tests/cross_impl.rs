//! B1's explicit accept criterion: "cross-implementation read/write test (write with
//! Node, read with Rust, and vice versa) passes" (`.ai/specs/2026-08-15-rust-tui-refactor-plan.md`).
//!
//! Each fixture script under `tests/cross_impl/` imports the REAL TypeScript source in
//! `packages/cezar/src/` directly (via `tsx`, no build step) and runs one real read or
//! write against a throwaway `CEZ_HOME`. This crate then does the other half in Rust and
//! asserts the two implementations agree — not against a hand-maintained JSON fixture
//! that could quietly drift from Node's actual behavior, but against Node's live output.
//!
//! Skips (rather than fails) when `node_modules/.bin/tsx` isn't present, so a checkout
//! without `npm install` doesn't break `cargo test`; CI runs `npm ci` before `cargo test`
//! (`.github/workflows/ci.yml`), so there this test is not optional.

use std::path::{Path, PathBuf};
use std::process::Command;

use coducktor_core::paths::EnvSource;
use coducktor_core::workspace::agent_accounts::{self, AgentAccountStore};
use coducktor_core::workspace::config::{self, WorkspaceConfig};

/// A fixed env snapshot — `coducktor_core::paths::test_env` is `#[cfg(test)]`-only inside
/// the library, so an external integration test crate (this file) needs its own.
struct FixedEnv(std::collections::BTreeMap<String, String>);

impl FixedEnv {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl EnvSource for FixedEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/coducktor-core is two levels under the repo root")
        .to_path_buf()
}

fn tsx_bin() -> Option<PathBuf> {
    let bin = repo_root().join("node_modules/.bin/tsx");
    bin.exists().then_some(bin)
}

/// Runs a `tests/cross_impl/<script>` fixture with `home_dir` as argv[2]. Panics on a
/// nonzero exit (a real failure, not a skip condition) or non-UTF8 stdout.
fn run_fixture(tsx: &Path, script: &str, home_dir: &Path) -> String {
    let output = Command::new(tsx)
        .arg(format!("crates/coducktor-core/tests/cross_impl/{script}"))
        .arg(home_dir)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn tsx for {script}: {err}"));
    assert!(
        output.status.success(),
        "{script} exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("fixture stdout is UTF-8")
}

/// `Some(tsx)` when `node_modules/.bin/tsx` is present; the caller prints a skip notice
/// and returns early otherwise (see the module doc comment for why this skips rather
/// than fails).
fn require_tsx() -> Option<PathBuf> {
    let bin = tsx_bin();
    if bin.is_none() {
        eprintln!(
            "skipping: node_modules/.bin/tsx not found — run `npm install` to enable this test"
        );
    }
    bin
}

#[test]
fn node_writes_workspace_config_and_rust_reads_it() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    run_fixture(tsx, "write_workspace_config.ts", dir.path());

    let env = FixedEnv::new(&[("DUCK_HOME", dir.path().to_str().unwrap())]);
    let config = config::load_workspace_config(&dir.path().join("config.json"), &env);

    assert_eq!(config.resources.max_parallel, 9);
    assert_eq!(
        config.resources.monitoring_wake_interval_minutes, None,
        "explicit null preserved"
    );
    assert_eq!(config.resources.memory_limit_mb, Some(4096));
    assert_eq!(config.composer_defaults.autonomous, Some(true));
    assert_eq!(
        config.disabled_providers,
        vec![
            coducktor_contract::Runner::Claude,
            coducktor_contract::Runner::Pi
        ],
        "deduped and canonical-ordered by Node's own writer",
    );
    assert_eq!(
        config.agent_defaults.runner,
        Some(coducktor_contract::RunnerSelection::Codex)
    );
    assert_eq!(
        config
            .agent_defaults
            .models
            .as_ref()
            .and_then(|m| m.codex.as_deref()),
        Some("gpt-cross-impl"),
    );
    assert!(config.quota_routing.enabled);
    assert_eq!(config.quota_routing.codex.long_window_stop_at_percent, 77.0);
    assert_eq!(config.projects.len(), 1);
    assert_eq!(config.projects[0].id, "cross-impl");
    assert_eq!(
        config.projects[0].tags.as_deref(),
        Some(&["storefront".to_owned()][..])
    );
    assert_eq!(
        config
            .extra
            .get("fromTheFutureNode")
            .and_then(|v| v.as_str()),
        Some("kept-by-rust"),
        "an unknown top-level key from a newer Node writer must survive Rust's parse",
    );
}

#[test]
fn rust_writes_workspace_config_and_node_reads_it() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let env = FixedEnv::new(&[("DUCK_HOME", dir.path().to_str().unwrap())]);

    config::merge_write_workspace_config(&path, &env, |cfg: &mut WorkspaceConfig| {
        cfg.resources.max_parallel = 6;
        cfg.resources.worktree_retention_default = 0;
        cfg.quota_routing.enabled = true;
        cfg.agent_defaults.runner = Some(coducktor_contract::RunnerSelection::Pi);
        cfg.disabled_providers = vec![coducktor_contract::Runner::OpenCode];
        cfg.projects.push(config::WorkspaceProject {
            id: "from-rust".to_owned(),
            root: "/repo/from-rust".to_owned(),
            name: "From Rust".to_owned(),
            added_at: String::new(),
            last_opened_at: String::new(),
            source: config::ProjectSource::Local,
            max_parallel: None,
            tags: None,
            extra: Default::default(),
        });
        cfg.extra.insert(
            "fromTheFutureRust".to_owned(),
            serde_json::json!("kept-by-node"),
        );
    })
    .unwrap();

    let stdout = run_fixture(tsx, "read_workspace_config.ts", dir.path());
    let node: serde_json::Value = serde_json::from_str(&stdout).expect("Node printed valid JSON");

    assert_eq!(node["resources"]["maxParallel"], 6);
    assert_eq!(node["resources"]["worktreeRetentionDefault"], 0);
    assert_eq!(node["quotaRouting"]["enabled"], true);
    assert_eq!(node["agentDefaults"]["runner"], "pi");
    assert_eq!(node["disabledProviders"], serde_json::json!(["opencode"]));
    assert_eq!(node["projects"][0]["id"], "from-rust");
    assert_eq!(
        node["fromTheFutureRust"], "kept-by-node",
        "Node's passthrough kept Rust's unknown key"
    );
}

#[test]
fn node_writes_agent_accounts_and_rust_reads_it() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    run_fixture(tsx, "write_agent_accounts.ts", dir.path());

    let store = agent_accounts::load_agent_accounts(&dir.path().join("agent-accounts.json"));
    assert_eq!(store.accounts.len(), 1);
    assert_eq!(store.accounts[0].id, "work");
    assert_eq!(store.accounts[0].config_dir, "~/.claude-work");
    assert_eq!(store.defaults.claude.as_deref(), Some("work"));
    assert_eq!(
        store.selection_for(Some("/repo/cross-impl"), coducktor_contract::Runner::Claude),
        Some("work"),
    );
    assert_eq!(
        store
            .extra
            .get("fromTheFutureNode")
            .and_then(|v| v.as_str()),
        Some("kept-by-rust"),
    );
}

#[test]
fn rust_writes_agent_accounts_and_node_reads_it() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-accounts.json");

    agent_accounts::merge_write_agent_accounts(&path, |store: &mut AgentAccountStore| {
        store.accounts.push(agent_accounts::AgentAccount {
            id: "from-rust".to_owned(),
            provider: coducktor_contract::Runner::Codex,
            config_dir: "~/.codex-from-rust".to_owned(),
            label: "From Rust".to_owned(),
            added_at: String::new(),
            extra: Default::default(),
        });
        store.defaults.codex = Some("from-rust".to_owned());
    })
    .unwrap();

    let stdout = run_fixture(tsx, "read_agent_accounts.ts", dir.path());
    let node: serde_json::Value = serde_json::from_str(&stdout).expect("Node printed valid JSON");

    assert_eq!(node["accounts"][0]["id"], "from-rust");
    assert_eq!(node["accounts"][0]["provider"], "codex");
    assert_eq!(node["defaults"]["codex"], "from-rust");
}
