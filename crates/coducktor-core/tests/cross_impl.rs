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

use coducktor_contract::events::RunEvent;
use coducktor_contract::runs::{RunRecord, StepState};
use coducktor_contract::{RunStatus, Runner, RunnerSelection, StepKind, StepStatus};
use coducktor_core::git::worktree;
use coducktor_core::paths::EnvSource;
use coducktor_core::runs::{events, store as run_store};
use coducktor_core::todos;
use coducktor_core::workflows::load as workflows_load;
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

/// Like [`run_fixture`], but for fixtures that take more than one positional argument
/// (the git-layer scripts: a repo path plus a ref/run id/branch).
fn run_fixture_args(tsx: &Path, script: &str, args: &[&str]) -> String {
    let output = Command::new(tsx)
        .arg(format!("crates/coducktor-core/tests/cross_impl/{script}"))
        .args(args)
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

/// B2's accept criterion: "tests against real files written by the Node version pass."
#[test]
fn node_writes_a_run_and_events_and_rust_reads_them() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    let run_id = run_fixture(tsx, "write_runs_and_events.ts", dir.path())
        .trim()
        .to_owned();

    // Node's fixture left the run `running` on a flushed-but-not-reopened store, and
    // hand-patched the legacy `claude-cli` runner id (#547) onto the saved index — both
    // exercise real Node output the Rust loader has to make sense of on its own.
    let runs = run_store::load_run_index(&run_store::index_path(dir.path()), false);
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.id, run_id);
    assert_eq!(run.title, "Cross-impl task");
    assert_eq!(run.task, "do the cross-impl thing");
    assert_eq!(
        run.runner,
        Some(Runner::Claude),
        "the legacy claude-cli id folds to claude"
    );
    assert_eq!(run.requested_runner, Some(RunnerSelection::Auto));
    assert_eq!(
        run.status,
        RunStatus::Failed,
        "reconcile_loaded_run marks a live-looking status interrupted"
    );
    assert_eq!(
        run.error.as_deref(),
        Some("interrupted — cezar process exited during the run")
    );
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].backend, Some(Runner::Claude));
    assert_eq!(
        run.steps[0].status,
        StepStatus::Failed,
        "a running step is interrupted along with its run"
    );

    let events = events::read_events(&events::events_path(dir.path(), &run_id));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1.0);
    assert_eq!(
        events[0].extra.get("text").and_then(|v| v.as_str()),
        Some("hello from node")
    );
    assert_eq!(
        events[1].extra.get("text").and_then(|v| v.as_str()),
        Some("a second line")
    );
}

/// The other half of B2's accept criterion: files Rust writes parse identically under the
/// real Node readers — `run-index.ts::readRunIndexFromDisk` and `RunStore::readEvents`.
#[test]
fn rust_writes_a_run_and_events_and_node_reads_them() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();

    let run = RunRecord {
        id: "rust-run".to_owned(),
        title: "From Rust".to_owned(),
        workflow: "quick-task".to_owned(),
        task: "do the rust thing".to_owned(),
        runner: Some(Runner::Codex),
        requested_runner: Some(RunnerSelection::Codex),
        status: RunStatus::Running, // left live on purpose — Node's reconcile must catch it
        created_at: "2026-08-16T00:00:00.000Z".to_owned(),
        tokens_used: 12.0,
        archived: false,
        steps: vec![StepState {
            id: "task".to_owned(),
            name: "Do the task".to_owned(),
            kind: StepKind::Agent,
            status: StepStatus::Running,
            iterations: 1.0,
            tokens_used: 12.0,
            input_tokens: None,
            output_tokens: None,
            usage_invocations_started: None,
            usage_invocations_observed: None,
            usage_turns_started: None,
            usage_turns_recorded: None,
            usage_invocation_epoch: None,
            started_at: None,
            finished_at: None,
            error: None,
            session_id: None,
            backend: Some(Runner::Codex),
            requested_runner: None,
            profile_id: None,
            reasoning_effort: None,
            cost_usd: None,
            model_identity: None,
        }],
        ..Default::default()
    };
    run_store::write_run_index(
        &run_store::index_path(dir.path()),
        std::slice::from_ref(&run),
    )
    .unwrap();

    let events_path = events::events_path(dir.path(), "rust-run");
    let mut extra = serde_json::Map::new();
    extra.insert("text".to_owned(), serde_json::json!("hello from rust"));
    events::append_event(
        &events_path,
        &RunEvent {
            seq: 1.0,
            ts: "2026-08-16T00:00:01.000Z".to_owned(),
            step_id: Some("task".to_owned()),
            event_type: "message".to_owned(),
            extra,
        },
    )
    .unwrap();

    let stdout = run_fixture(tsx, "read_runs_index.ts", dir.path());
    let node: serde_json::Value = serde_json::from_str(&stdout).expect("Node printed valid JSON");

    assert_eq!(node["runs"].as_array().map(Vec::len), Some(1));
    let node_run = &node["runs"][0];
    assert_eq!(node_run["id"], "rust-run");
    assert_eq!(node_run["title"], "From Rust");
    assert_eq!(node_run["runner"], "codex");
    assert_eq!(
        node_run["status"], "failed",
        "Node's own reconcileLoadedRun catches the live status Rust wrote"
    );
    assert_eq!(
        node_run["error"], "interrupted — cezar process exited during the run",
        "Node's reconcile message, produced by Node itself — not echoed by Rust"
    );
    assert_eq!(node_run["steps"][0]["status"], "failed");

    assert_eq!(node["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(node["events"][0]["seq"], 1);
    assert_eq!(node["events"][0]["text"], "hello from rust");
}

// B3's git-layer cross-impl tests, below. Repos are language-neutral, so unlike the
// JSON-file fixtures above, the fixture repo is built directly in Rust with `git` — no need
// to round-trip the setup through a `.ts` script. Scoped to two representative surfaces
// rather than all four the plan sketched (`resolveBaseRef`'s local/origin/stale matrix, and
// `createWorktree`'s cross-implementation reuse): `git-worktree.test.ts`/
// `git-diff-base.test.ts`/`autosave-conflict-guard.test.ts` are already ported byte-for-byte
// as `coducktor_core::git`'s own inline unit tests (real git binary, same scenarios,
// `cargo test -p coducktor-core`), which is the plan's actual accept criterion ("behavior
// matches the Node shell-out implementation on the existing test fixtures"). These two
// additionally confirm the TWO implementations agree with each OTHER, not just with a
// hand-ported copy of the same oracle.

fn run_git(args: &[&str], cwd: &std::path::Path) {
    assert!(
        Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed in {}",
        cwd.display()
    );
}

/// Mirrors `git-worktree.test.ts`'s `fixtureRepo()`: a `main`-branch repo with one commit.
fn init_fixture_repo(root: &Path) {
    run_git(&["init", "-q", "-b", "main"], root);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    run_git(&["add", "-A"], root);
    run_git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-q",
            "-m",
            "base",
        ],
        root,
    );
}

/// `resolveBaseRef`/`resolve_base_ref` must agree on which ref a worktree forks from —
/// getting this wrong is the "phantom 142k-line diff" bug the TS doc comment describes.
/// Exercises the plain (no origin) case and the stale-local-base case, matching
/// `git-worktree.test.ts`'s own matrix.
#[test]
fn resolve_base_ref_agrees_between_node_and_rust() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();

    let repo = tempfile::tempdir().unwrap();
    init_fixture_repo(repo.path());

    let node_plain = run_fixture_args(
        tsx,
        "resolve_base_ref.ts",
        &[repo.path().to_str().unwrap(), "main"],
    );
    let node_plain: serde_json::Value = serde_json::from_str(node_plain.trim()).unwrap();
    let rust_plain = worktree::resolve_base_ref(repo.path(), "main");
    assert_eq!(node_plain.as_str(), rust_plain.as_deref());
    assert_eq!(rust_plain.as_deref(), Some("main"));

    // Wire an origin remote, then advance it so the repo's local `main` goes stale.
    let remote = tempfile::tempdir().unwrap();
    init_fixture_repo(remote.path());
    run_git(
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
        repo.path(),
    );
    run_git(&["fetch", "-q", "origin"], repo.path());
    std::fs::write(remote.path().join("more.txt"), "more\n").unwrap();
    run_git(&["add", "-A"], remote.path());
    run_git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-q",
            "-m",
            "advance",
        ],
        remote.path(),
    );
    run_git(&["fetch", "-q", "origin"], repo.path());

    let node_stale = run_fixture_args(
        tsx,
        "resolve_base_ref.ts",
        &[repo.path().to_str().unwrap(), "main"],
    );
    let node_stale: serde_json::Value = serde_json::from_str(node_stale.trim()).unwrap();
    let rust_stale = worktree::resolve_base_ref(repo.path(), "main");
    assert_eq!(node_stale.as_str(), rust_stale.as_deref());
    assert_eq!(
        rust_stale.as_deref(),
        Some("origin/main"),
        "a stale local base must lose to origin"
    );
}

/// `createWorktree`/`create_worktree` must agree on the idempotency ladder: whichever
/// implementation registers a task's worktree first, the OTHER must recognize and reuse it
/// rather than erroring or double-creating — this is what lets Phase B's cutover (spec §11,
/// B11) run the two servers side by side against the same run without corrupting each
/// other's worktrees.
#[test]
fn create_worktree_reuse_agrees_between_node_and_rust() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();

    let repo = tempfile::tempdir().unwrap();
    init_fixture_repo(repo.path());

    // Node creates; Rust must reuse the exact same registration.
    let node_created = run_fixture_args(
        tsx,
        "create_worktree.ts",
        &[repo.path().to_str().unwrap(), "node-first", "main"],
    );
    let node_created: serde_json::Value = serde_json::from_str(node_created.trim()).unwrap();
    let rust_reused = worktree::create_worktree(repo.path(), "node-first", "main").unwrap();
    assert_eq!(node_created["path"], rust_reused.path);
    assert_eq!(node_created["branch"], rust_reused.branch);
    assert_eq!(node_created["branch"], "duck/node-fir"); // first 8 chars of "node-first"
    assert_eq!(node_created["baseBranch"], rust_reused.base_branch);

    // Rust creates; Node must reuse the exact same registration.
    let rust_created = worktree::create_worktree(repo.path(), "rust-first", "main").unwrap();
    let node_reused = run_fixture_args(
        tsx,
        "create_worktree.ts",
        &[repo.path().to_str().unwrap(), "rust-first", "main"],
    );
    let node_reused: serde_json::Value = serde_json::from_str(node_reused.trim()).unwrap();
    assert_eq!(node_reused["path"], rust_created.path);
    assert_eq!(node_reused["branch"], rust_created.branch);
    assert_eq!(node_reused["baseBranch"], rust_created.base_branch);
}

// B4's fixtures, below: `todos.json` (a shared file both implementations read AND write
// during Phase B, same as `runs.json`) and workflow YAML (not written by either
// implementation at runtime — it's repo content — but parsed by two different YAML
// libraries, `yaml` on the Node side and `serde_yaml_ng` here, which is exactly the kind of
// place two "compliant" YAML parsers can quietly disagree).

/// B4's accept criterion applied to `todos.json`: Node writes a realistic mix (a
/// server-assigned id, a bare agent-written entry with no id at all, and one malformed
/// entry with an empty `summary`), Rust reads it back and must salvage exactly what
/// `readRaw`'s own per-entry `safeParse` would.
#[test]
fn node_writes_todos_and_rust_reads_them() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    run_fixture(tsx, "write_todos.ts", dir.path());

    let data_dir = dir.path().join(".ai/coducktor");
    let items = todos::read_todos(&data_dir);
    assert_eq!(items.len(), 2, "the empty-summary entry is skipped");
    let with_id = items.iter().find(|t| t.id == "node-1").unwrap();
    assert_eq!(with_id.summary, "a real follow-up");
    assert_eq!(with_id.runnable, Some(true));
    let from_agent = items
        .iter()
        .find(|t| t.summary == "from an agent, no id yet")
        .unwrap();
    assert!(!from_agent.id.is_empty(), "Rust assigns a missing id too");
    assert_eq!(
        from_agent.pr_url.as_deref(),
        Some("https://github.com/o/r/pull/1")
    );
}

/// The other half: Rust writes `todos.json` (including an entry with no `startedTaskId`,
/// the field Node's own "▶ Run" sets later), Node's real `readTodos` reads it back.
#[test]
fn rust_writes_todos_and_node_reads_them() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join(".ai/coducktor");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        todos::todos_path(&data_dir),
        serde_json::to_string_pretty(&serde_json::json!([
            { "id": "rust-1", "summary": "from rust", "runnable": false },
        ]))
        .unwrap(),
    )
    .unwrap();

    let stdout = run_fixture(tsx, "read_todos.ts", dir.path());
    let node: serde_json::Value = serde_json::from_str(&stdout).expect("Node printed valid JSON");
    assert_eq!(node[0]["id"], "rust-1");
    assert_eq!(node[0]["summary"], "from rust");
    assert_eq!(node[0]["runnable"], false);
}

/// B4's accept criterion applied to workflow YAML: the same on-disk `.ai/coducktor/
/// workflows/*.yaml` — one plain-steps file, one `skills:` shorthand file, one file with an
/// unresolvable `onFail.retry` — loaded by Node's `yaml`-backed loader and Rust's
/// `serde_yaml_ng`-backed one, asserting they resolve to the same catalog and flag the same
/// file as an issue.
#[test]
fn node_and_rust_agree_on_the_same_workflow_yaml() {
    let Some(tsx) = require_tsx() else { return };
    let tsx = tsx.as_path();
    let dir = tempfile::tempdir().unwrap();
    let workflows_dir = dir.path().join(".ai/coducktor/workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::write(
        workflows_dir.join("review.yaml"),
        "name: review\ndescription: Review a PR\nsteps:\n  - id: check\n    command: npm test\n  - id: fix\n    prompt: '{{task}}'\n    onFail:\n      retry: check\n      max: 3\n",
    )
    .unwrap();
    std::fs::write(
        workflows_dir.join("stack.yml"),
        "name: stack\nskills:\n  - \" om-a \"\n  - om-b\n",
    )
    .unwrap();
    std::fs::write(
        workflows_dir.join("broken.yaml"),
        "name: broken\nsteps:\n  - id: a\n    prompt: '{{task}}'\n    onFail:\n      retry: nope\n",
    )
    .unwrap();

    let stdout = run_fixture(tsx, "load_workflows.ts", dir.path());
    let node: serde_json::Value = serde_json::from_str(&stdout).expect("Node printed valid JSON");
    let node_names: Vec<&str> = node["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();

    let (workflows, issues) = workflows_load::load_workflows(dir.path());
    let rust_names: Vec<&str> = workflows.iter().map(|w| w.name.as_str()).collect();

    assert_eq!(node_names, rust_names);
    assert_eq!(node_names, ["quick-task", "review", "stack"]);
    assert_eq!(node["issues"].as_array().map(Vec::len), Some(1));
    assert_eq!(issues.len(), 1);
    assert_eq!(node["issues"][0]["path"], issues[0].path);

    let node_stack = node["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "stack")
        .unwrap();
    let rust_stack = workflows.iter().find(|w| w.name == "stack").unwrap();
    assert_eq!(
        node_stack["steps"][0]["id"], "om-a",
        "both trim the skill name"
    );
    assert_eq!(rust_stack.steps[0].id, "om-a");
    assert_eq!(
        node_stack["steps"].as_array().unwrap().len(),
        rust_stack.steps.len()
    );
}
