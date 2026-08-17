//! The non-interactive `coducktor`/`duck` subcommands: `run`, `init`, `usage`, `doctor`, and
//! `projects`. Console wording is intentionally terminal-native.

use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use coducktor_client::InProcessEngine;
use coducktor_contract::{BackendCheckName, RunStatus, RunnerSelection};
use coducktor_core::paths::{ProcessEnv, workspace_config_path};
use coducktor_core::workflows::run::{RunManager, StartRunInput};
use coducktor_core::workflows::{load::load_workflows, types::quick_task_workflow};
use coducktor_core::workspace::config::ProjectSource;
use coducktor_core::workspace::projects;
use coducktor_runners::session_factory::DefaultSessionFactory;

use crate::cli::ProjectsCommand;

/// Resolve the requested directory, preferring its enclosing Git repository root over an
/// arbitrary subdirectory. Falls back to the directory itself when it is not inside a Git repo.
pub fn resolve_repo_root(explicit: Option<&Path>) -> PathBuf {
    let start = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let output = ShellCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&start)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if root.is_empty() {
                start
            } else {
                PathBuf::from(root)
            }
        }
        _ => start,
    }
}

// ---- run (headless) -----------------------------------------------------------------------

/// `coducktor run "<task>"` — headless execution. Returns the process exit code: 0 for `done`/
/// `review`, 1 otherwise.
pub async fn run_command(
    repo_root: PathBuf,
    task: String,
    workflow_name: Option<String>,
    model: Option<String>,
) -> i32 {
    run_command_with_factory(
        repo_root,
        task,
        workflow_name,
        model,
        DefaultSessionFactory::new(),
    )
}

/// The testable core of [`run_command`] — takes its `SessionFactory` explicitly so a test gets
/// deterministic backend resolution (`DUCK_DRY_RUN=1`, a fixed `host_env`) without mutating the
/// real process environment, which `#[test]`s running in parallel in this same binary cannot do
/// safely.
fn run_command_with_factory(
    repo_root: PathBuf,
    task: String,
    workflow_name: Option<String>,
    model: Option<String>,
    factory: DefaultSessionFactory,
) -> i32 {
    if task.trim().is_empty() {
        eprintln!("usage: coducktor run \"<task>\" [--workflow name] [--model model]");
        return 1;
    }

    let (mut workflows, issues) = load_workflows(&repo_root);
    for issue in &issues {
        eprintln!("! skipped {}: {}", issue.path, issue.message);
    }
    if workflows
        .iter()
        .all(|workflow| workflow.name != "quick-task")
    {
        workflows.push(quick_task_workflow());
    }
    let name = workflow_name.unwrap_or_else(|| "quick-task".to_owned());
    let Some(workflow) = workflows.iter().find(|workflow| workflow.name == name) else {
        eprintln!(
            "unknown workflow: {name} (available: {})",
            workflows
                .iter()
                .map(|workflow| workflow.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return 1;
    };

    let mut manager = RunManager::for_repo(&repo_root);
    manager.set_session_factory(factory);
    manager.subscribe_events(|notification| print_run_event(&notification.event));

    let input = StartRunInput {
        task,
        model,
        runner: Some(RunnerSelection::Claude),
        ..StartRunInput::default()
    };
    let record = match manager.start_run(workflow, input) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("  ✗ {error}");
            return 1;
        }
    };

    if record.status == RunStatus::Review {
        println!(
            "\n  changes ready for review on branch {} — inspect them with `coducktor`",
            record.branch.as_deref().unwrap_or("?")
        );
    }
    println!(
        "\nrun {:?} — {} tokens",
        record.status, record.tokens_used as i64
    );
    match record.status {
        RunStatus::Done | RunStatus::Review => 0,
        _ => 1,
    }
}

/// Print normalized run events with terminal-friendly formatting.
fn print_run_event(event: &coducktor_contract::RunEvent) {
    let text = |key: &str| -> String {
        event
            .extra
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    match event.event_type.as_str() {
        "text" | "check-output" => println!("{}", text("text")),
        "tool-call" => {
            let input = event
                .extra
                .get("input")
                .map(preview_json)
                .unwrap_or_default();
            println!("  → {} {input}", text("tool"));
        }
        "tool-result" => println!("  ← {}", first_line(&text("result"))),
        "step-start" => {
            let name = text("name");
            let iteration = event.extra.get("iteration").and_then(|v| v.as_f64());
            match iteration {
                Some(iteration) if iteration > 1.0 => {
                    println!("\n── step: {name} (attempt {})", iteration as i64)
                }
                _ => println!("\n── step: {name}"),
            }
        }
        "note" | "lifecycle" => println!("  · {}", text("message")),
        "error" => eprintln!("  ✗ {}", text("message")),
        _ => {}
    }
}

fn preview_json(value: &serde_json::Value) -> String {
    let rendered = value.to_string();
    if rendered.len() > 120 {
        format!("{}…", &rendered[..117])
    } else {
        rendered
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > 120 {
        let head: String = line.chars().take(117).collect();
        format!("{head}…")
    } else {
        line.to_owned()
    }
}

// ---- init ----------------------------------------------------------------------------------

/// `coducktor init` — scaffold `.ai/coducktor/{workflows,skills}` with one worked example each.
pub fn init_command(repo_root: &Path) {
    let workflows_dir = repo_root.join(".ai/coducktor/workflows");
    let skills_dir = repo_root.join(".ai/coducktor/skills");
    let _ = std::fs::create_dir_all(&workflows_dir);
    let _ = std::fs::create_dir_all(&skills_dir);

    let examples = [
        (
            workflows_dir.join("fix-and-verify.yaml"),
            "name: fix-and-verify\ndescription: Implement the task, then run your test command; on failure the agent retries with the failing output.\nsteps:\n  - id: implement\n    name: Implement\n    prompt: \"{{task}}\"\n  - id: verify\n    name: Verify\n    command: \"echo 'replace me with: npm test / yarn test / pytest'\"\n    onFail:\n      retry: implement\n      max: 2\n",
        ),
        (
            skills_dir.join("project-conventions.md"),
            "---\nname: project-conventions\ndescription: House rules the agent should follow in this repo.\n---\n\n# Project conventions\n\n- Describe your stack, style and testing conventions here.\n- Reference this skill from a workflow step via `skill: project-conventions`.\n",
        ),
    ];

    for (path, content) in examples {
        if path.exists() {
            println!("  = {} (exists, left untouched)", path.display());
        } else if std::fs::write(&path, content).is_ok() {
            println!("  + {}", path.display());
        }
    }
    ensure_data_gitignore(repo_root);
    println!("\nDone. Start the cockpit with: coducktor");
}

/// Keep run data out of the user's repo history; workflows and skills stay committable.
fn ensure_data_gitignore(repo_root: &Path) {
    const WANTED: &[&str] = &[
        "runs.json",
        "runs.json.tmp",
        "runs/",
        "worktrees/",
        "tmp/",
        "todos.json",
        "todos.json.tmp",
    ];
    let dir = repo_root.join(".ai/coducktor");
    let path = dir.join(".gitignore");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = current.split('\n').collect();
    let missing: Vec<&str> = WANTED
        .iter()
        .copied()
        .filter(|wanted| !lines.contains(wanted))
        .collect();
    if missing.is_empty() {
        return;
    }
    let glue = if !current.is_empty() && !current.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let mut file = current;
    file.push_str(glue);
    file.push_str(&missing.join("\n"));
    file.push('\n');
    let _ = std::fs::write(&path, file);
}

// ---- usage ---------------------------------------------------------------------------------

/// `coducktor usage` — quota telemetry is not available in this build.
pub fn usage_command() -> i32 {
    eprintln!("coducktor usage: quota telemetry is not available in this build");
    1
}

// ---- doctor -------------------------------------------------------------------------------

/// Inspect the local installation without opening a TUI or a listening socket.
///
/// The update row is intentionally source-first: this build has no package registry or
/// background updater, so the honest update action is to pull the checkout and rerun
/// `install.sh`.
pub async fn doctor_command(repo_root: PathBuf, json: bool) -> i32 {
    let engine = InProcessEngine::new(repo_root, env!("CARGO_PKG_VERSION"));
    let health = match engine.diagnostic_health().await {
        Ok(health) => health,
        Err(error) => {
            eprintln!("coducktor doctor: {error}");
            return 1;
        }
    };

    let agent_checks = health.checks.iter().filter(|check| {
        matches!(
            check.name,
            BackendCheckName::Claude
                | BackendCheckName::Codex
                | BackendCheckName::OpenCode
                | BackendCheckName::Pi
        )
    });
    let agent_count = agent_checks.clone().count();
    let available_agents = agent_checks.filter(|check| check.available).count();

    if json {
        let report = serde_json::json!({
            "version": health.version,
            "update": {
                "available": false,
                "hint": "source-first install: run git pull and ./install.sh",
            },
            "repoRoot": health.repo_root,
            "checks": health.checks,
        });
        match serde_json::to_string_pretty(&report) {
            Ok(rendered) => println!("{rendered}"),
            Err(error) => {
                eprintln!("coducktor doctor: could not render JSON: {error}");
                return 1;
            }
        }
    } else {
        println!("coducktor doctor");
        println!("  version: {}", health.version);
        println!("  update: source-first install — run `git pull && ./install.sh`");
        println!("  repo: {}", health.repo_root);
        println!("  agent CLIs: {available_agents}/{agent_count} available");
        for check in &health.checks {
            let mark = if check.available { "✓" } else { "✗" };
            let version = check
                .version
                .as_deref()
                .map(|version| format!(" ({version})"))
                .unwrap_or_default();
            let hint = check
                .hint
                .as_deref()
                .map(|hint| format!(" — {hint}"))
                .unwrap_or_default();
            println!(
                "  {mark} {}{version}{hint}",
                backend_check_label(check.name)
            );
        }
    }

    if available_agents > 0 { 0 } else { 1 }
}

fn backend_check_label(name: BackendCheckName) -> &'static str {
    match name {
        BackendCheckName::Claude => "claude",
        BackendCheckName::Codex => "codex",
        BackendCheckName::OpenCode => "opencode",
        BackendCheckName::Pi => "pi",
        BackendCheckName::Gh => "gh",
        BackendCheckName::Git => "git",
    }
}

// ---- projects --------------------------------------------------------------------------------

/// `coducktor projects [list|add [<dir>]|remove <id>|rm <id>]` — the terminal project-registry
/// commands. They operate directly on the per-user workspace config and need no service.
pub fn projects_command(repo_root: &Path, action: Option<ProjectsCommand>) -> i32 {
    projects_command_at(&workspace_config_path(&ProcessEnv), repo_root, action)
}

/// The testable core of [`projects_command`] — takes the registry file path explicitly so a
/// test operates on a tempdir's own registry instead of the real `~/.coducktor/config.json`.
fn projects_command_at(path: &Path, repo_root: &Path, action: Option<ProjectsCommand>) -> i32 {
    match action {
        None | Some(ProjectsCommand::List) => projects_list(path),
        Some(ProjectsCommand::Add { dir }) => {
            let root = dir.unwrap_or_else(|| repo_root.to_path_buf());
            projects_add(path, &root)
        }
        Some(ProjectsCommand::Remove { id }) => projects_remove(path, &id),
    }
}

fn projects_list(path: &Path) -> i32 {
    let entries = projects::list_projects(path, &ProcessEnv);
    if entries.is_empty() {
        println!("\n  no projects registered yet");
        println!("  start coducktor in a repo or add one: coducktor projects add <dir>\n");
        return 0;
    }
    println!();
    for project in &entries {
        let status = projects::probe_status(Path::new(&project.root));
        let (mark, label) = match status {
            coducktor_contract::ProjectStatus::Missing => ("✗", "missing".to_owned()),
            coducktor_contract::ProjectStatus::NotGit => ("·", "not a git repo".to_owned()),
            coducktor_contract::ProjectStatus::Ok => ("✓", "ok".to_owned()),
        };
        let tags = project
            .tags
            .as_ref()
            .filter(|tags| !tags.is_empty())
            .map(|tags| format!("  [{}]", tags.join(" ")))
            .unwrap_or_default();
        println!("  {mark} {}  {label}  {}{tags}", project.id, project.root);
    }
    println!(
        "\n  {} project(s) — registry: {}\n",
        entries.len(),
        path.display()
    );
    0
}

fn projects_add(path: &Path, root: &Path) -> i32 {
    if !root.is_dir() {
        eprintln!("not a directory: {}", root.display());
        return 1;
    }
    if !projects::should_register_project(root, &ProcessEnv) {
        eprintln!(
            "refusing to register {} — coducktor task worktrees and your home directory are not projects",
            root.display()
        );
        return 1;
    }
    let known: std::collections::HashSet<String> = projects::list_projects(path, &ProcessEnv)
        .into_iter()
        .map(|project| project.id)
        .collect();
    match projects::register_project(path, &ProcessEnv, root, ProjectSource::Local) {
        Ok(entry) => {
            if known.contains(&entry.id) {
                println!("  = {} (already registered)  {}", entry.id, entry.root);
            } else {
                println!("  + {}  {}", entry.id, entry.root);
            }
            0
        }
        Err(error) => {
            eprintln!("failed to register {}: {error}", root.display());
            1
        }
    }
}

fn projects_remove(path: &Path, id: &str) -> i32 {
    match projects::remove_project(path, &ProcessEnv, id) {
        Ok(true) => {
            println!(
                "  - {id} (registry entry only — the repo and its .ai/coducktor/ are untouched)"
            );
            0
        }
        Ok(false) => {
            eprintln!("unknown project: {id}");
            1
        }
        Err(error) => {
            eprintln!("failed to remove {id}: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_core::workspace::config::load_workspace_config;
    use std::collections::BTreeMap;

    #[test]
    fn resolve_repo_root_finds_the_git_toplevel_from_a_subdirectory() {
        let repo = tempfile::tempdir().unwrap();
        assert!(
            ShellCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
        let nested = repo.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let root = resolve_repo_root(Some(&nested));
        assert_eq!(
            root.canonicalize().unwrap(),
            repo.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_repo_root_falls_back_to_the_given_directory_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        // A tempdir is (almost always) not itself inside a git work tree.
        let root = resolve_repo_root(Some(dir.path()));
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn init_command_scaffolds_the_example_workflow_skill_and_gitignore() {
        let repo = tempfile::tempdir().unwrap();
        init_command(repo.path());

        let workflow = repo
            .path()
            .join(".ai/coducktor/workflows/fix-and-verify.yaml");
        let skill = repo
            .path()
            .join(".ai/coducktor/skills/project-conventions.md");
        assert!(workflow.is_file());
        assert!(skill.is_file());
        assert!(
            std::fs::read_to_string(&workflow)
                .unwrap()
                .contains("fix-and-verify")
        );

        let gitignore =
            std::fs::read_to_string(repo.path().join(".ai/coducktor/.gitignore")).unwrap();
        for wanted in ["runs.json", "worktrees/", "todos.json"] {
            assert!(gitignore.contains(wanted), "gitignore missing {wanted:?}");
        }

        // Idempotent: a second run leaves the files alone rather than erroring or duplicating.
        let before = std::fs::read_to_string(&workflow).unwrap();
        init_command(repo.path());
        assert_eq!(std::fs::read_to_string(&workflow).unwrap(), before);
    }

    #[test]
    fn projects_command_add_list_remove_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let registry = home.path().join("config.json");
        let repo = tempfile::tempdir().unwrap();

        let added = projects_command_at(
            &registry,
            repo.path(),
            Some(ProjectsCommand::Add { dir: None }),
        );
        assert_eq!(added, 0);
        let config = load_workspace_config(&registry, &ProcessEnv);
        assert_eq!(config.projects.len(), 1);
        let id = config.projects[0].id.clone();

        assert_eq!(projects_command_at(&registry, repo.path(), None), 0);

        let removed = projects_command_at(
            &registry,
            repo.path(),
            Some(ProjectsCommand::Remove { id: id.clone() }),
        );
        assert_eq!(removed, 0);
        assert!(
            load_workspace_config(&registry, &ProcessEnv)
                .projects
                .is_empty()
        );

        let unknown =
            projects_command_at(&registry, repo.path(), Some(ProjectsCommand::Remove { id }));
        assert_eq!(unknown, 1);
    }

    #[test]
    fn usage_command_reports_not_yet_implemented_and_fails() {
        assert_eq!(usage_command(), 1);
    }

    /// A fake "repo" carrying just enough of the real tree's shape
    /// (`fixtures/scripts/mock-claude.mjs`) for `DefaultSessionFactory`'s dry-run path
    /// resolution to find it, without touching the real dev checkout's `.ai/coducktor/`.
    fn fake_repo_with_mock_claude() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let scripts_dir = repo.path().join("fixtures/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let real_mock = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/scripts/mock-claude.mjs");
        std::fs::copy(&real_mock, scripts_dir.join("mock-claude.mjs")).unwrap();
        repo
    }

    fn dry_run_factory() -> DefaultSessionFactory {
        let mut env = BTreeMap::new();
        env.insert("DUCK_DRY_RUN".to_owned(), "1".to_owned());
        DefaultSessionFactory::with_env(env)
    }

    #[test]
    fn run_command_reaches_done_and_exits_zero_against_the_dry_run_mock() {
        let repo = fake_repo_with_mock_claude();
        let code = run_command_with_factory(
            repo.path().to_path_buf(),
            "investigate the login redirect bug mock:done".to_owned(),
            None,
            None,
            dry_run_factory(),
        );
        assert_eq!(code, 0);
    }

    #[test]
    fn run_command_reports_an_unknown_workflow_and_exits_nonzero() {
        let repo = fake_repo_with_mock_claude();
        let code = run_command_with_factory(
            repo.path().to_path_buf(),
            "do it".to_owned(),
            Some("no-such-workflow".to_owned()),
            None,
            dry_run_factory(),
        );
        assert_eq!(code, 1);
    }

    #[test]
    fn run_command_rejects_an_empty_task() {
        let repo = fake_repo_with_mock_claude();
        let code = run_command_with_factory(
            repo.path().to_path_buf(),
            "   ".to_owned(),
            None,
            None,
            dry_run_factory(),
        );
        assert_eq!(code, 1);
    }
}
