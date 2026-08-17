//! Project registry operations over `~/.coducktor/config.json`. Ported from
//! `packages/cezar/src/workspace/projects.ts` for the `cezar projects` CLI subcommand (B10) —
//! the terminal twin of Settings → Projects, working with no server running.
//!
//! **Scope cut (B10):** `projects.ts`'s per-root git/forge probe (branch, forge kind, repo URL,
//! all behind a 5s TTL cache) is not ported — it needs `server/git.ts`'s `getRepoInfo` and
//! `server/forge/`, neither ported to this crate (B3's own scope note already deferred
//! `server/{git,git-changes}.ts` as "server-route-adjacent logic"). [`probe_status`] here is a
//! plain filesystem check (`missing`/`not-git`/`ok`, no cache, no branch/forge/repo-url) — good
//! enough for a headless listing; `coducktor-server`'s own `list_projects` route handler already
//! has its own richer (if independently-derived, not shared with this module) probe for the HTTP
//! API / TUI project switcher. `normalizeProjectTags`/`cezar projects tag` is not ported either —
//! a secondary UX affordance, not part of the protected CLI surface (spec §1.4 names `serve`/
//! `run`/`init`/`usage`/`projects`, not its subcommands).
//!
//! **Known duplication, not resolved here:** `coducktor-server`'s own route handlers
//! (`register_project`/`list_projects`/`remove_project` in `crates/coducktor-server/src/lib.rs`)
//! already independently reimplement the registry read-modify-write and slug allocation this
//! module also implements — that duplication predates B10 (B9's route-family work inlined it
//! rather than reaching for a core module that didn't exist yet) and is not this step's file to
//! fix; a future chunk should have those handlers delegate here instead.

use std::path::{Path, PathBuf};

use coducktor_contract::ProjectStatus;

use super::config::{
    ProjectSource, WorkspaceProject, load_workspace_config, merge_write_workspace_config,
};
use crate::paths::{EnvSource, real_home_dir};
use crate::time::now_iso8601;

/// Slugs the allocator must never hand out: `default` is the reserved alias for the boot
/// project, the rest are the cockpit shell's own top-level path segments.
pub const RESERVED_PROJECT_IDS: &[&str] = &["default", "new", "settings", "api", "p", "assets"];

/// Slug length cap — mirrors `PROJECT_ID_RE` (1 head char + up to 63 more).
const SLUG_MAX: usize = 64;

/// `basename(root)` → slug base: lowercase, runs of `[^a-z0-9-]` become one `-`, edge dashes
/// trimmed. A degenerate basename falls back to `project` rather than ever escaping the slug
/// shape.
fn slug_base(root: &Path) -> String {
    let base = root
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out = String::with_capacity(base.len());
    let mut last_dash = false;
    for ch in base.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(SLUG_MAX).collect();
    if trimmed.is_empty() {
        "project".to_owned()
    } else {
        trimmed
    }
}

/// Allocate a unique slug for `root`: the slug base, deduplicated against `taken` ids AND the
/// reserved ids with a numeric suffix (`api`, `api-2`, `api-3`, …). Suffixed candidates stay
/// within the 64-char cap by truncating the base, never the suffix.
pub fn allocate_project_slug<'a>(root: &Path, taken: impl Iterator<Item = &'a str>) -> String {
    let mut used: std::collections::HashSet<&str> = RESERVED_PROJECT_IDS.iter().copied().collect();
    for id in taken {
        used.insert(id);
    }
    let base = slug_base(root);
    if !used.contains(base.as_str()) {
        return base;
    }
    let mut n = 2;
    loop {
        let suffix = format!("-{n}");
        let keep = SLUG_MAX.saturating_sub(suffix.len());
        let head: String = base.chars().take(keep).collect();
        let candidate = format!("{head}{suffix}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

/// Realpath-normalize a root: resolves symlinks and drops trailing slashes, so every spelling of
/// the same directory dedupes to one registry entry. A path that cannot be canonicalized
/// (doesn't exist yet, unreadable) degrades to itself, unchanged — callers of [`register_project`]
/// guard existence.
fn normalize_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// True when `path` sits inside a coducktor task worktree (`…/.ai/coducktor/worktrees/…`).
fn is_inside_task_worktree(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    components.windows(3).any(|window| {
        window[0].as_os_str() == ".ai"
            && window[1].as_os_str() == "coducktor"
            && window[2].as_os_str() == "worktrees"
    })
}

/// Registration guard: auto-registration is suppressed — the process still serves the folder
/// normally, it just doesn't pollute the registry — when `repo_root` is inside any task worktree
/// (checked on both the raw and realpath'd spelling) or is the user's home directory itself
/// (realpath-compared).
pub fn should_register_project(repo_root: &Path, env: &dyn EnvSource) -> bool {
    let real = normalize_root(repo_root);
    if is_inside_task_worktree(&real) || is_inside_task_worktree(repo_root) {
        return false;
    }
    let home = normalize_root(&real_home_dir(env));
    real != home
}

/// Register `root` in the workspace registry (idempotent). Known root (by realpath) → bump its
/// `last_opened_at` and return the existing entry, id and all. Unknown → allocate a slug and
/// append a new entry via merge-write.
pub fn register_project(
    path: &Path,
    env: &dyn EnvSource,
    root: &Path,
    source: ProjectSource,
) -> std::io::Result<WorkspaceProject> {
    let real = normalize_root(root);
    let real_string = real.to_string_lossy().into_owned();
    let now = now_iso8601();
    let mut result: Option<WorkspaceProject> = None;
    merge_write_workspace_config(path, env, |config| {
        if let Some(existing) = config
            .projects
            .iter_mut()
            .find(|project| project.root == real_string)
        {
            existing.last_opened_at = now.clone();
            result = Some(existing.clone());
            return;
        }
        let id = allocate_project_slug(
            &real,
            config.projects.iter().map(|project| project.id.as_str()),
        );
        let name = real
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| real_string.clone());
        let entry = WorkspaceProject {
            id,
            root: real_string.clone(),
            name,
            added_at: now.clone(),
            last_opened_at: now.clone(),
            source,
            max_parallel: None,
            tags: None,
            extra: serde_json::Map::new(),
        };
        config.projects.push(entry.clone());
        result = Some(entry);
    })?;
    Ok(result.expect("the mutator above always sets result"))
}

/// Registry entries in stored order. Callers wanting `status`/`branch` call [`probe_status`]
/// per entry themselves (no TTL cache here — see the module doc's scope-cut note).
pub fn list_projects(path: &Path, env: &dyn EnvSource) -> Vec<WorkspaceProject> {
    load_workspace_config(path, env).projects
}

/// A plain filesystem check: `missing` if the root doesn't exist or isn't a directory, `not-git`
/// if it exists but has no `.git`, else `ok`. No branch/forge/repo-url (scope cut, module doc).
pub fn probe_status(root: &Path) -> ProjectStatus {
    if !root.is_dir() {
        return ProjectStatus::Missing;
    }
    if root.join(".git").exists() {
        ProjectStatus::Ok
    } else {
        ProjectStatus::NotGit
    }
}

/// Remove `id` from the registry. Returns false when no such entry exists. Pure unregistration:
/// nothing inside the repo (worktrees, `.ai/coducktor/`, run history) is touched.
pub fn remove_project(path: &Path, env: &dyn EnvSource, id: &str) -> std::io::Result<bool> {
    let mut removed = false;
    merge_write_workspace_config(path, env, |config| {
        let before = config.projects.len();
        config.projects.retain(|project| project.id != id);
        removed = config.projects.len() != before;
    })?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_env::FixedEnv;
    use tempfile::tempdir;

    fn env() -> FixedEnv {
        FixedEnv::default()
    }

    #[test]
    fn allocate_project_slug_lowercases_and_collapses_non_alnum_runs() {
        let slug = allocate_project_slug(Path::new("/repos/My Cool App!!"), std::iter::empty());
        assert_eq!(slug, "my-cool-app");
    }

    #[test]
    fn allocate_project_slug_falls_back_to_project_for_a_degenerate_basename() {
        let slug = allocate_project_slug(Path::new("/repos/日本語"), std::iter::empty());
        assert_eq!(slug, "project");
    }

    #[test]
    fn allocate_project_slug_numbers_past_a_reserved_or_taken_id() {
        assert_eq!(
            allocate_project_slug(Path::new("/repos/api"), std::iter::empty()),
            "api-2"
        );
        assert_eq!(
            allocate_project_slug(Path::new("/repos/demo"), ["demo"].into_iter()),
            "demo-2"
        );
    }

    #[test]
    fn is_inside_task_worktree_matches_the_documented_shape() {
        assert!(is_inside_task_worktree(Path::new(
            "/home/u/repo/.ai/coducktor/worktrees/abc123"
        )));
        assert!(!is_inside_task_worktree(Path::new("/home/u/repo")));
    }

    #[test]
    fn register_project_is_idempotent_by_realpath_and_bumps_last_opened_at() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let path = home.path().join("config.json");
        let env = env();

        let first = register_project(&path, &env, repo.path(), ProjectSource::Local).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = register_project(&path, &env, repo.path(), ProjectSource::Local).unwrap();

        assert_eq!(first.id, second.id);
        let all = list_projects(&path, &env);
        assert_eq!(all.len(), 1, "a re-register must not duplicate the entry");
        assert!(second.last_opened_at >= first.last_opened_at);
    }

    #[test]
    fn register_project_allocates_a_fresh_slug_per_distinct_root() {
        let home = tempdir().unwrap();
        let repo_a = tempdir().unwrap();
        let repo_b = tempdir().unwrap();
        let path = home.path().join("config.json");
        let env = env();

        let a = register_project(&path, &env, repo_a.path(), ProjectSource::Local).unwrap();
        let b = register_project(&path, &env, repo_b.path(), ProjectSource::Local).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(list_projects(&path, &env).len(), 2);
    }

    #[test]
    fn remove_project_unregisters_by_id_and_reports_unknown_ids() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let path = home.path().join("config.json");
        let env = env();

        let entry = register_project(&path, &env, repo.path(), ProjectSource::Local).unwrap();
        assert!(!remove_project(&path, &env, "does-not-exist").unwrap());
        assert!(remove_project(&path, &env, &entry.id).unwrap());
        assert!(list_projects(&path, &env).is_empty());
    }

    #[test]
    fn probe_status_reflects_the_filesystem() {
        let missing = tempdir().unwrap().path().join("gone");
        assert_eq!(probe_status(&missing), ProjectStatus::Missing);

        let not_git = tempdir().unwrap();
        assert_eq!(probe_status(not_git.path()), ProjectStatus::NotGit);

        let repo = tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        assert_eq!(probe_status(repo.path()), ProjectStatus::Ok);
    }

    #[test]
    fn should_register_project_refuses_home_itself() {
        let home = tempdir().unwrap();
        let env = FixedEnv::new(&[("HOME", &home.path().to_string_lossy())]);
        assert!(!should_register_project(home.path(), &env));
    }

    #[test]
    fn should_register_project_refuses_a_task_worktree() {
        let env = env();
        let worktree = tempdir().unwrap();
        let nested = worktree.path().join(".ai/coducktor/worktrees/abc123");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(!should_register_project(&nested, &env));
    }

    #[test]
    fn should_register_project_allows_an_ordinary_repo() {
        let home = tempdir().unwrap();
        let env = FixedEnv::new(&[("HOME", &home.path().to_string_lossy())]);
        let repo = tempdir().unwrap();
        assert!(should_register_project(repo.path(), &env));
    }
}
