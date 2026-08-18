//! Ordered migrations for per-user workspace state.
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

use std::io;
use std::path::Path;

use crate::paths::{self, EnvSource};

use super::config::{WorkspaceConfig, merge_write_workspace_config};

struct Migration {
    /// `schema_version` this migration produces.
    to: u32,
    /// Stable id used in a failure message, e.g. `"001-workspace-config"`.
    id: &'static str,
    run: fn(&MigrationContext) -> io::Result<()>,
}

struct MigrationContext<'a> {
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
    match std::fs::rename(old, new) {
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

/// The home state-dir rename migration 002 performs, also run unconditionally before the
/// migration chain. Repository directories are deliberately never migration targets.
pub fn migrate_state_dirs(_boot_repo_root: Option<&Path>, env: &dyn EnvSource) -> Vec<String> {
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
    messages
}

/// Migration 001 — `schemaVersion 0 → 1`: create `~/.coducktor/config.json` with
/// defaults if absent. Project state is never read or written during startup.
fn migration_001(ctx: &MigrationContext) -> io::Result<()> {
    let config_path = paths::workspace_config_path(ctx.env);
    merge_write_workspace_config(&config_path, ctx.env, |_| {})?;
    Ok(())
}

/// Migration 002 — `schemaVersion 1 → 2`, the product rename: move the on-disk state
/// dirs from their legacy names to the current ones. Registered as a normal
/// migration too (on top of `run_migrations` calling `migrate_state_dirs`
/// unconditionally first) so the framework's record bumps `schema_version` to 2 and
/// re-running it is the same idempotent no-op.
fn migration_002(ctx: &MigrationContext) -> io::Result<()> {
    migrate_state_dirs(None, ctx.env);
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
    let ctx = MigrationContext { env };

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
    fn startup_migrations_never_read_or_write_the_repository() {
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
        assert_eq!(config.resources.max_parallel, 2);
        assert!(repo.path().join(".ai/coducktor/config.json").exists());
    }

    #[test]
    fn repository_settings_are_never_imported_even_across_reruns() {
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
        // A repository-owned file remains entirely outside startup migration scope.
        fs::write(
            repo.path().join(".ai/coducktor/config.json"),
            r#"{"maxParallel": 3}"#,
        )
        .unwrap();
        run_migrations(Some(repo.path()), &env);
        let config = load_workspace_config(&dir.path().join("config.json"), &env);
        assert_eq!(config.resources.max_parallel, 2);
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
    fn a_pre_rename_repository_directory_is_left_untouched_on_boot() {
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
        assert!(old_repo.exists());
        assert_eq!(
            fs::read_to_string(real_home.path().join(".coducktor/ui-state.json")).unwrap(),
            r#"{"notifications":{"enabled":true}}"#
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
