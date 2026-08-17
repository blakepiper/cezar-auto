//! Ordered workspace migrations for user and repository state. Migrations are deliberately
//! small and config-files-only; run state (`runs.json`, NDJSON) remains readable in place.
//! They are:
//!
//! - **idempotent** — every migration is safe to re-run after a crash mid-way;
//! - **additive** — never deletes or rewrites the user's per-repo files;
//! - **non-blocking** — a failing migration reports ONE message and boot proceeds
//!   degraded with in-memory defaults; it is never a boot failure;
//! - **concurrency-safe** — every write takes the same read-modify-write + atomic-rename
//!   path as all workspace writes, and two processes racing the same idempotent step
//!   converge.
//!
//! Every diagnostic comes back as a `String` in [`MigrationRunOutcome::messages`]. The caller
//! decides whether it belongs on stderr, in a TUI notice, or in CLI output.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use crate::paths::{self, EnvSource};

use super::config::{WorkspaceConfig, merge_write_workspace_config};
use super::ui_state::merge_write_workspace_ui_state;

struct Migration {
    /// `schema_version` this migration produces.
    to: u32,
    /// Stable id used in a failure message, e.g. `"001-workspace-config"`.
    id: &'static str,
    run: fn(&MigrationContext) -> io::Result<()>,
}

struct MigrationContext<'a> {
    boot_repo_root: Option<&'a Path>,
    env: &'a dyn EnvSource,
}

const LEGACY_STATE_DIR: &str = concat!(".", "ce", "zar");
const LEGACY_PROJECTS_DIR: &str = "~/cezar/projects";
const CURRENT_PROJECTS_DIR: &str = "~/coducktor/projects";

/// All known migrations, in ascending `to` order.
const WORKSPACE_MIGRATIONS: &[Migration] = &[
    Migration {
        to: 1,
        id: "001-workspace-config",
        run: migration_001,
    },
    Migration {
        to: 2,
        id: "002-coducktor-state-dirs",
        run: migration_002,
    },
    Migration {
        to: 3,
        id: "003-coducktor-projects-dir",
        run: migration_003,
    },
];

fn read_raw_object(path: &Path) -> Option<Map<String, Value>> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value.as_object().cloned()
}

/// One on-disk state-dir rename. Idempotent and never destructive: old absent → nothing
/// to do; both present → the new dir wins, the stray old one is reported but never
/// deleted; old present/new absent → rename and report.
fn migrate_state_dir(old: &Path, new: &Path, label: &str) -> Option<String> {
    if !old.exists() {
        return None;
    }
    if new.exists() {
        return Some(format!(
            "found both {} and {} — using {}; remove {} once you no longer need it",
            old.display(),
            new.display(),
            new.display(),
            old.display(),
        ));
    }
    match fs::rename(old, new) {
        Ok(()) => Some(format!(
            "moved {label} state from {} to {}",
            old.display(),
            new.display()
        )),
        Err(err) => Some(format!(
            "could not move {label} state from {} to {} ({err})",
            old.display(),
            new.display(),
        )),
    }
}

/// The two state-dir renames migration 002 performs, also run unconditionally BEFORE the
/// migration chain (see [`run_migrations`]) — on a pre-rename install, migration 001 must
/// not create a fresh `.ai/coducktor` config while the real one is still under the legacy path.
pub fn migrate_state_dirs(boot_repo_root: Option<&Path>, env: &dyn EnvSource) -> Vec<String> {
    let mut messages = Vec::new();
    // Only meaningful when neither home override is set — an explicit `DUCK_HOME` is the
    // location (tests/containers/an already-migrated install), so
    // there is no old spelling to move.
    if env.get("DUCK_HOME").filter(|v| !v.is_empty()).is_none() {
        let real_home = paths::real_home_dir(env);
        if let Some(message) = migrate_state_dir(
            &real_home.join(LEGACY_STATE_DIR),
            &real_home.join(".coducktor"),
            "home",
        ) {
            messages.push(message);
        }
    }
    if let Some(repo_root) = boot_repo_root
        && let Some(message) = migrate_state_dir(
            &repo_root.join(".ai").join(LEGACY_STATE_DIR),
            &repo_root.join(".ai/coducktor"),
            "repo",
        )
    {
        messages.push(message);
    }
    messages
}

#[derive(Default)]
struct RepoResourceKeys {
    max_parallel: Option<u64>,
    memory_limit_mb: Option<u64>,
}

/// The boot repo's `.ai/coducktor/config.json` resource keys, read RAW so only values the
/// user explicitly set are imported — a defaulted value must not masquerade as a
/// preference.
fn read_repo_resource_keys(repo_root: &Path) -> RepoResourceKeys {
    let raw = read_raw_object(&repo_root.join(".ai/coducktor/config.json")).unwrap_or_default();
    let bounded = |key: &str, lo: i64, hi: i64| {
        raw.get(key).and_then(|v| {
            let n = v.as_i64()?;
            (n >= lo && n <= hi).then_some(n as u64)
        })
    };
    RepoResourceKeys {
        max_parallel: bounded("maxParallel", 1, 16),
        memory_limit_mb: bounded("memoryLimitMb", 0, 1_048_576),
    }
}

/// Migration 001 — `schemaVersion 0 → 1`: create `~/.coducktor/config.json` with
/// defaults if absent; when booting inside a repo, import its `maxParallel`/
/// `memoryLimitMb` into workspace `resources`, and its `appearance`/`notifications`
/// ui-state keys into `~/.coducktor/ui-state.json`. Keys already set globally are NEVER
/// overwritten — presence is checked against the RAW global file (before defaults are
/// applied), which is what makes a crash-interrupted re-run safe. Every per-repo file is
/// left untouched.
fn migration_001(ctx: &MigrationContext) -> io::Result<()> {
    let config_path = paths::workspace_config_path(ctx.env);
    let raw_global = read_raw_object(&config_path);
    let raw_resources = raw_global
        .as_ref()
        .and_then(|o| o.get("resources"))
        .and_then(Value::as_object);
    let imported = ctx
        .boot_repo_root
        .map(read_repo_resource_keys)
        .unwrap_or_default();

    merge_write_workspace_config(&config_path, ctx.env, |config| {
        if let Some(max_parallel) = imported.max_parallel
            && raw_resources.and_then(|r| r.get("maxParallel")).is_none()
        {
            config.resources.max_parallel = max_parallel;
        }
        if let Some(memory_limit_mb) = imported.memory_limit_mb
            && raw_resources.and_then(|r| r.get("memoryLimitMb")).is_none()
        {
            config.resources.memory_limit_mb = Some(memory_limit_mb);
        }
    })?;

    let Some(repo_root) = ctx.boot_repo_root else {
        return Ok(());
    };
    let repo_ui_state =
        read_raw_object(&repo_root.join(".ai/coducktor/ui-state.json")).unwrap_or_default();
    // Validated with the DESTINATION's own field schemas — a hand-edited value that
    // doesn't parse is simply not imported, never a failed boot.
    let appearance = repo_ui_state.get("appearance").cloned().filter(|v| {
        serde_json::from_value::<coducktor_contract::workspace::Appearance>(v.clone()).is_ok()
    });
    let notifications = repo_ui_state.get("notifications").cloned().filter(|v| {
        serde_json::from_value::<coducktor_contract::workspace::NotificationsUiState>(v.clone())
            .is_ok()
    });
    if appearance.is_none() && notifications.is_none() {
        return Ok(()); // nothing to import — don't create the file
    }

    let ui_state_path = paths::workspace_ui_state_path(ctx.env);
    merge_write_workspace_ui_state(&ui_state_path, |state| {
        if state.appearance.is_none()
            && let Some(value) = appearance
        {
            state.appearance = serde_json::from_value(value).ok();
        }
        if state.notifications.is_none()
            && let Some(value) = notifications
        {
            state.notifications = serde_json::from_value(value).ok();
        }
    })?;
    Ok(())
}

/// Migration 002 — `schemaVersion 1 → 2`, the product rename: move the on-disk state
/// dirs from their legacy names to the current ones. Registered as a normal
/// migration too (on top of `run_migrations` calling `migrate_state_dirs`
/// unconditionally first) so the framework's record bumps `schema_version` to 2 and
/// re-running it is the same idempotent no-op.
fn migration_002(ctx: &MigrationContext) -> io::Result<()> {
    migrate_state_dirs(ctx.boot_repo_root, ctx.env);
    Ok(())
}

/// Migration 003 — replace the old product's default checkout root. The exact legacy default is
/// the only value rewritten; arbitrary project roots remain durable user configuration.
fn migration_003(ctx: &MigrationContext) -> io::Result<()> {
    let config_path = paths::workspace_config_path(ctx.env);
    merge_write_workspace_config(&config_path, ctx.env, |config| {
        if config.projects_dir == LEGACY_PROJECTS_DIR {
            config.projects_dir = CURRENT_PROJECTS_DIR.to_owned();
        }
    })?;
    Ok(())
}

/// What [`run_migrations`] did — the final `schema_version` and every diagnostic message
/// collected along the way (state-dir rename notes, or the one message a failing
/// migration produces before the chain stops).
pub struct MigrationRunOutcome {
    pub schema_version: u32,
    pub messages: Vec<String>,
}

/// Run every pending workspace migration — call at boot before anything else touches
/// `~/.coducktor`. Reads `schema_version` (absent/bad → 0, meaning "run everything" —
/// safe because every migration is idempotent), runs each migration with `to > current`
/// in ascending order, and persists the new `schema_version` after EACH one, so a crash
/// resumes exactly where it left off. A failing migration stops the chain (later
/// migrations may depend on earlier ones); the caller boots degraded on in-memory
/// defaults. Never panics.
pub fn run_migrations(boot_repo_root: Option<&Path>, env: &dyn EnvSource) -> MigrationRunOutcome {
    // State-dir rename FIRST: on a pre-rename install, migration 001's config write must
    // land in the migrated home rather than create a fresh `.coducktor` alongside the
    // user's real `.coducktor` config.
    let mut messages = migrate_state_dirs(boot_repo_root, env);
    let config_path = paths::workspace_config_path(env);
    let mut current = super::config::load_workspace_config(&config_path, env).schema_version;
    let ctx = MigrationContext {
        boot_repo_root,
        env,
    };

    for migration in WORKSPACE_MIGRATIONS {
        if migration.to <= current {
            continue;
        }
        let outcome = (migration.run)(&ctx).and_then(|()| {
            merge_write_workspace_config(&config_path, env, |config: &mut WorkspaceConfig| {
                config.schema_version = config.schema_version.max(migration.to);
            })
        });
        match outcome {
            Ok(written) => current = written.schema_version,
            Err(err) => {
                messages.push(format!(
                    "workspace migration {} failed ({err}) — booting with in-memory defaults",
                    migration.id,
                ));
                break;
            }
        }
    }

    MigrationRunOutcome {
        schema_version: current,
        messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::FixedEnv;
    use crate::workspace::config::load_workspace_config;
    use std::fs;

    fn env_for(home: &Path) -> FixedEnv {
        FixedEnv::new(&[("DUCK_HOME", home.to_str().unwrap())])
    }

    #[test]
    fn migrations_are_registered_in_ascending_order() {
        let ordered = WORKSPACE_MIGRATIONS
            .windows(2)
            .all(|pair| pair[0].to < pair[1].to);
        assert!(ordered);
    }

    #[test]
    fn the_migration_list_is_frozen_at_one_through_three() {
        // Pinned deliberately: a purely-additive schema key
        // does not get a reflexive no-op migration.
        let versions: Vec<u32> = WORKSPACE_MIGRATIONS.iter().map(|m| m.to).collect();
        assert_eq!(versions, vec![1, 2, 3]);
    }

    #[test]
    fn a_fresh_home_ends_at_schema_version_three_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let outcome = run_migrations(None, &env);
        assert_eq!(outcome.schema_version, 3);
        let config = load_workspace_config(&dir.path().join("config.json"), &env);
        assert_eq!(config.schema_version, 3);
    }

    #[test]
    fn rerunning_migrations_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        run_migrations(None, &env);
        let outcome = run_migrations(None, &env);
        assert_eq!(outcome.schema_version, 3);
        assert!(outcome.messages.is_empty());
    }

    #[test]
    fn repo_resource_keys_import_only_when_the_global_key_is_unset() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".ai/coducktor")).unwrap();
        fs::write(
            repo.path().join(".ai/coducktor/config.json"),
            r#"{"maxParallel": 9}"#,
        )
        .unwrap();

        run_migrations(Some(repo.path()), &env);
        let config = load_workspace_config(&dir.path().join("config.json"), &env);
        assert_eq!(config.resources.max_parallel, 9);
    }

    #[test]
    fn a_value_already_set_globally_is_never_overwritten_even_across_a_simulated_crash_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".ai/coducktor")).unwrap();
        fs::write(
            repo.path().join(".ai/coducktor/config.json"),
            r#"{"maxParallel": 9}"#,
        )
        .unwrap();

        run_migrations(Some(repo.path()), &env);
        // Simulate the repo's own config changing between runs — schema_version already
        // 3, but even a hypothetical re-import must not clobber the imported value.
        fs::write(
            repo.path().join(".ai/coducktor/config.json"),
            r#"{"maxParallel": 3}"#,
        )
        .unwrap();
        run_migrations(Some(repo.path()), &env);
        let config = load_workspace_config(&dir.path().join("config.json"), &env);
        assert_eq!(config.resources.max_parallel, 9);
    }

    #[test]
    fn state_dirs_are_renamed_before_migration_001_reads_the_config() {
        let real_home = tempfile::tempdir().unwrap();
        let env = FixedEnv::new(&[("HOME", real_home.path().to_str().unwrap())]);
        let old_dir = real_home.path().join(LEGACY_STATE_DIR);
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(
            old_dir.join("config.json"),
            r#"{"schemaVersion": 1, "resources": {"maxParallel": 11}}"#,
        )
        .unwrap();

        let outcome = run_migrations(None, &env);
        assert_eq!(outcome.schema_version, 3);
        assert!(!old_dir.exists(), "the old dir is renamed, not copied");
        let config = load_workspace_config(&real_home.path().join(".coducktor/config.json"), &env);
        assert_eq!(
            config.resources.max_parallel, 11,
            "the migrated config is the one read"
        );
    }

    #[test]
    fn a_pre_rename_repo_and_home_are_both_migrated_on_boot() {
        let real_home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let env = FixedEnv::new(&[("HOME", real_home.path().to_str().unwrap())]);
        let old_home = real_home.path().join(LEGACY_STATE_DIR);
        let old_repo = repo.path().join(".ai").join(LEGACY_STATE_DIR);
        fs::create_dir_all(&old_home).unwrap();
        fs::create_dir_all(&old_repo).unwrap();
        fs::write(
            old_home.join("ui-state.json"),
            r#"{"notifications":{"enabled":true}}"#,
        )
        .unwrap();
        fs::write(old_repo.join("runs.json"), "[]").unwrap();

        let outcome = run_migrations(Some(repo.path()), &env);

        assert_eq!(outcome.schema_version, 3);
        assert!(!old_home.exists());
        assert!(!old_repo.exists());
        assert_eq!(
            fs::read_to_string(real_home.path().join(".coducktor/ui-state.json")).unwrap(),
            r#"{"notifications":{"enabled":true}}"#
        );
        assert_eq!(
            fs::read_to_string(repo.path().join(".ai/coducktor/runs.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn an_explicit_home_override_skips_the_home_dir_rename() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let messages = migrate_state_dirs(None, &env);
        assert!(messages.is_empty());
    }

    #[test]
    fn legacy_checkout_root_migrates_to_the_current_default() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        fs::write(
            dir.path().join("config.json"),
            r#"{"schemaVersion":2,"projectsDir":"~/cezar/projects"}"#,
        )
        .unwrap();

        let outcome = run_migrations(None, &env);

        assert_eq!(outcome.schema_version, 3);
        assert_eq!(
            load_workspace_config(&dir.path().join("config.json"), &env).projects_dir,
            "~/coducktor/projects"
        );
    }
}
