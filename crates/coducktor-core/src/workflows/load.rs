//! The workflow catalog loader. Mirrors `packages/coducktor/src/workflows/load.ts`.
//!
//! Loads the built-in `quick-task` plus every `.ai/coducktor/workflows/*.{yaml,yml}` in the
//! repo. File workflows win name collisions with built-ins. Invalid files are reported,
//! never fatal.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use coducktor_contract::workflows::{WorkflowDef, WorkflowLoadIssue, WorkflowSource};

use super::types::{parse_workflow_file_doc, quick_task_workflow, steps_issue};

pub const WORKFLOWS_DIR: &str = ".ai/coducktor/workflows";

fn is_workflow_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
}

fn load_one_workflow_file(path: &Path) -> Result<WorkflowDef, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_yaml_ng::from_str(&raw).map_err(|e| e.to_string())?;
    // `skills:` shorthand files become plain agent steps here (spec 012).
    let (name, description, steps) = parse_workflow_file_doc(&value)?;
    // Steps referenced by onFail.retry must exist and come earlier; ids unique.
    if let Some(issue) = steps_issue(&steps) {
        return Err(issue);
    }
    Ok(WorkflowDef {
        name,
        description,
        steps,
        source: WorkflowSource::File,
        path: Some(path.to_string_lossy().into_owned()),
    })
}

/// Load the workflow catalog: the built-in `quick-task` plus every
/// `.ai/coducktor/workflows/*.{yaml,yml}` in the repo. File workflows win name collisions
/// with built-ins. Invalid files are reported, never fatal.
pub fn load_workflows(repo_root: &Path) -> (Vec<WorkflowDef>, Vec<WorkflowLoadIssue>) {
    let dir = repo_root.join(WORKFLOWS_DIR);
    let mut entries: Vec<_> = fs::read_dir(&dir) // no workflows dir — built-ins only
        .map(|rd| rd.filter_map(Result::ok).collect())
        .unwrap_or_default();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut issues = Vec::new();
    let mut from_files = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !is_workflow_file(&path) {
            continue;
        }
        match load_one_workflow_file(&path) {
            Ok(def) => from_files.push(def),
            Err(message) => issues.push(WorkflowLoadIssue {
                path: path.to_string_lossy().into_owned(),
                message,
            }),
        }
    }

    let file_names: HashSet<String> = from_files.iter().map(|w| w.name.clone()).collect();
    let mut workflows = from_files;
    let built_in = quick_task_workflow();
    if !file_names.contains(&built_in.name) {
        workflows.push(built_in);
    }
    workflows.sort_by(|a, b| a.name.cmp(&b.name));
    (workflows, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_workflows_dir_yields_only_the_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let (workflows, issues) = load_workflows(dir.path());
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "quick-task");
        assert_eq!(workflows[0].source, WorkflowSource::BuiltIn);
        assert!(issues.is_empty());
    }

    #[test]
    fn loads_a_steps_file_and_a_skills_shorthand_file() {
        let dir = tempfile::tempdir().unwrap();
        let workflows_dir = dir.path().join(WORKFLOWS_DIR);
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(
            workflows_dir.join("review.yaml"),
            "name: review\ndescription: Review a PR\nsteps:\n  - id: check\n    command: npm test\n  - id: fix\n    prompt: '{{task}}'\n    onFail:\n      retry: check\n",
        )
        .unwrap();
        fs::write(
            workflows_dir.join("stack.yml"),
            "name: stack\nskills:\n  - om-a\n  - om-b\n",
        )
        .unwrap();
        // Non-yaml files in the same dir are ignored.
        fs::write(workflows_dir.join("readme.txt"), "not a workflow").unwrap();

        let (workflows, issues) = load_workflows(dir.path());
        assert!(issues.is_empty(), "{issues:?}");
        let names: Vec<_> = workflows.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["quick-task", "review", "stack"]);
        let review = workflows.iter().find(|w| w.name == "review").unwrap();
        assert_eq!(review.source, WorkflowSource::File);
        assert_eq!(review.steps.len(), 2);
        let stack = workflows.iter().find(|w| w.name == "stack").unwrap();
        assert_eq!(stack.steps.len(), 2);
        assert_eq!(stack.steps[0].skill.as_deref(), Some("om-a"));
    }

    #[test]
    fn a_file_workflow_wins_a_name_collision_with_the_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let workflows_dir = dir.path().join(WORKFLOWS_DIR);
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(
            workflows_dir.join("quick-task.yaml"),
            "name: quick-task\nsteps:\n  - id: only\n    prompt: '{{task}}'\n",
        )
        .unwrap();

        let (workflows, _issues) = load_workflows(dir.path());
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].source, WorkflowSource::File);
    }

    #[test]
    fn an_invalid_file_is_reported_but_does_not_block_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let workflows_dir = dir.path().join(WORKFLOWS_DIR);
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(workflows_dir.join("broken.yaml"), "steps: [\n").unwrap(); // malformed YAML
        fs::write(
            workflows_dir.join("both.yaml"),
            "name: both\nsteps: []\nskills: []\n",
        )
        .unwrap();
        fs::write(
            workflows_dir.join("good.yaml"),
            "name: good\nsteps:\n  - id: a\n    prompt: '{{task}}'\n",
        )
        .unwrap();

        let (workflows, issues) = load_workflows(dir.path());
        assert_eq!(issues.len(), 2);
        let names: Vec<_> = workflows.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["good", "quick-task"]);
    }

    #[test]
    fn a_backwards_on_fail_retry_is_reported_as_an_issue_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let workflows_dir = dir.path().join(WORKFLOWS_DIR);
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(
            workflows_dir.join("loop.yaml"),
            "name: loop\nsteps:\n  - id: a\n    command: t\n    onFail:\n      retry: b\n  - id: b\n    prompt: '{{task}}'\n",
        )
        .unwrap();

        let (workflows, issues) = load_workflows(dir.path());
        assert_eq!(workflows.len(), 1, "only the built-in survives");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("earlier step"));
    }
}
