//! The `coducktor` binary's argument surface (spec §6.3, §10 A13).
//!
//! Phase A never re-implements the Node CLI's `serve`/`run`/`init`/`projects`
//! commands here — that split is Phase B's `B10` step (spec §11.1), operating on
//! the ported core/server crates once they exist. This binary only launches
//! the interactive TUI, so its surface is deliberately small: bare invocation and
//! an explicit `tui` subcommand behave identically, and three launch-time flags
//! (`--repo`, `--workflow`, `--model`) carry real, testable meaning into the first
//! frame rather than being decorative. `-p/--port` and `--no-open` — protected on
//! the Node CLI (`BACKWARD_COMPATIBILITY.md` §1) — are not reproduced here: spec
//! §1.4 waives both, since neither means anything without a server the TUI itself
//! opens a browser for.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use coducktor_contract::{ProjectListEntry, WorkflowDef};

/// `coducktor` — the terminal cockpit.
#[derive(Debug, Parser)]
#[command(name = "coducktor", version, about = "The coducktor terminal cockpit", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Open directly into this repo's project — must already be a registered
    /// project (add it from the TUI's project switcher first).
    #[arg(long, global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// Preselect a workflow on the New Task screen at launch.
    #[arg(long, global = true, value_name = "NAME")]
    pub workflow: Option<String>,

    /// Preselect a model on the New Task screen at launch.
    #[arg(long, global = true, value_name = "MODEL")]
    pub model: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Launch the interactive TUI — the default when no subcommand is given.
    Tui,
}

impl Cli {
    /// Parse `argv`, exiting the process on `--help`/`--version`/a bad flag —
    /// the same startup-only escape hatch `main.rs` already uses elsewhere
    /// (spec §0 Definition of Done footnote on `unwrap`/`expect` in `main.rs`).
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

/// Match `--repo`'s directory against the registered-projects list by canonical
/// path, so a symlink or a relative path resolves the same way a shell `cd` would.
/// Returns the matching project's id, or `None` if the directory isn't registered.
pub fn resolve_repo(registry: &[ProjectListEntry], repo: &Path) -> Option<String> {
    let target = repo.canonicalize().ok()?;
    registry
        .iter()
        .find(|entry| {
            Path::new(&entry.root)
                .canonicalize()
                .is_ok_and(|root| root == target)
        })
        .map(|entry| entry.id.clone())
}

/// Whether `--workflow <name>` names a workflow the resolved project actually has —
/// checked so a typo produces a notice instead of silently degrading to baseline
/// (`new_task_form::resolve_source`'s existing fallback would otherwise hide it).
pub fn workflow_known(workflows: &[WorkflowDef], name: &str) -> bool {
    workflows.iter().any(|workflow| workflow.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, root: &str) -> ProjectListEntry {
        ProjectListEntry {
            id: id.to_owned(),
            root: root.to_owned(),
            ..ProjectListEntry::default()
        }
    }

    #[test]
    fn bare_invocation_and_the_tui_subcommand_parse_identically() {
        let bare = Cli::try_parse_from(["coducktor"]).unwrap();
        let explicit = Cli::try_parse_from(["coducktor", "tui"]).unwrap();
        assert!(bare.command.is_none());
        assert!(matches!(explicit.command, Some(Command::Tui)));
        assert_eq!(bare.repo, explicit.repo);
    }

    #[test]
    fn repo_workflow_model_parse_on_bare_invocation() {
        let cli = Cli::try_parse_from([
            "coducktor",
            "--repo",
            "/tmp/some-repo",
            "--workflow",
            "quick-task",
            "--model",
            "sonnet",
        ])
        .unwrap();
        assert_eq!(cli.repo, Some(PathBuf::from("/tmp/some-repo")));
        assert_eq!(cli.workflow.as_deref(), Some("quick-task"));
        assert_eq!(cli.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn flags_are_global_and_also_parse_after_the_tui_subcommand() {
        let cli = Cli::try_parse_from(["coducktor", "tui", "--workflow", "quick-task"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Tui)));
        assert_eq!(cli.workflow.as_deref(), Some("quick-task"));
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["coducktor", "serve"]).is_err());
        assert!(Cli::try_parse_from(["coducktor", "run", "do the thing"]).is_err());
    }

    #[test]
    fn help_names_every_flag_this_binary_actually_supports() {
        let error = Cli::try_parse_from(["coducktor", "--help"]).unwrap_err();
        let rendered = error.to_string();
        for needle in ["--repo", "--workflow", "--model", "tui"] {
            assert!(
                rendered.contains(needle),
                "help text missing {needle:?}: {rendered}"
            );
        }
        // The waived flags (spec §1.4) must not resurface here.
        for needle in ["--port", "--no-open"] {
            assert!(
                !rendered.contains(needle),
                "help text unexpectedly has {needle:?}"
            );
        }
    }

    #[test]
    fn resolve_repo_matches_by_canonical_path() {
        let dir = std::env::current_dir().unwrap();
        let registry = vec![project("proj-a", &dir.to_string_lossy())];
        assert_eq!(resolve_repo(&registry, &dir), Some("proj-a".to_owned()));
    }

    #[test]
    fn resolve_repo_is_none_for_an_unregistered_directory() {
        let dir = std::env::current_dir().unwrap();
        let registry = vec![project("proj-a", "/definitely/not/this/dir")];
        assert_eq!(resolve_repo(&registry, &dir), None);
    }

    fn workflow(name: &str) -> WorkflowDef {
        WorkflowDef {
            name: name.to_owned(),
            description: None,
            steps: Vec::new(),
            source: coducktor_contract::WorkflowSource::File,
            path: None,
        }
    }

    #[test]
    fn workflow_known_matches_by_name_only() {
        let workflows = vec![workflow("quick-task")];
        assert!(workflow_known(&workflows, "quick-task"));
        assert!(!workflow_known(&workflows, "typo-task"));
        assert!(!workflow_known(&[], "quick-task"));
    }
}
