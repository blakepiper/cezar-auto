//! The slice of `packages/cezar/src/workflows/types.ts` that [`super::load`] needs: turning
//! a raw parsed YAML/JSON document into a validated [`WorkflowDef`], and the built-in
//! `quick-task` workflow it falls back to.
//!
//! `coducktor_contract::workflows` already carries every WIRE shape (`WorkflowDef`,
//! `WorkflowStepDef`, `WorkflowOnFail`) — ported at A1 from `packages/contract/src/
//! workflows.ts`, which `types.ts`'s own zod schemas are kept parity-checked against — so
//! this module adds only the validation and normalization `types.ts` layers on top of that
//! shape, scoped to exactly what the FILE LOADER exercises:
//!
//! - `skillsToSteps` / `normalizeWorkflowDoc` — resolving the `steps` XOR `skills`
//!   shorthand into plain steps.
//! - `stepsIssue` — the structural check (unique ids, backwards-only `onFail.retry`).
//! - The `workflowStepSchema`/`workflowFileSchema` `.refine()` rules a plain
//!   `#[derive(Deserialize)]` on the contract types can't express (schema base cases —
//!   wrong field types, an unknown `runner` enum value — are `derive`'s job already, so
//!   there is no `zod`-compat helper module needed here the way `runs::store` needed one).
//! - `QUICK_TASK_WORKFLOW`.
//!
//! **Deliberately not ported here:** `skillStackOf` (the inverse of `skillsToSteps`, used by
//! the compact-YAML-export UI, not the loader — and already independently reimplemented at
//! Phase A in `coducktor-tui`'s `screens/workflows.rs` since this crate had no
//! `workflows` module yet), `chainStepNote` and `DEFAULT_ALLOWED_TOOLS` (both consumed at
//! run EXECUTION time, i.e. `workflows::run` — B6 territory). Revisit consolidating the TUI's
//! copy of `skillStackOf` onto this crate's `WorkflowStepDef` re-export once B6 lands and
//! `coducktor-tui` has a reason to depend on more of `workflows` than just the loader.

use std::collections::HashSet;

use serde_json::Value;

use coducktor_contract::runs::StepKind;
use coducktor_contract::workflows::{WorkflowDef, WorkflowSource, WorkflowStepDef};

/// `skills: [a, b]` → agent steps, one per skill, each running `{{task}}`. Mirrors
/// `types.ts::skillsToSteps`.
pub fn skills_to_steps(skills: &[String]) -> Vec<WorkflowStepDef> {
    let mut used: HashSet<String> = HashSet::new();
    skills
        .iter()
        .map(|skill| {
            let mut id = skill.clone();
            let mut n = 2;
            while used.contains(&id) {
                id = format!("{skill}-{n}");
                n += 1;
            }
            used.insert(id.clone());
            WorkflowStepDef {
                id,
                name: Some(skill.clone()),
                prompt: Some("{{task}}".to_owned()),
                skill: Some(skill.clone()),
                model: None,
                runner: None,
                allowed_tools: None,
                bash_allowlist: None,
                command: None,
                on_fail: None,
            }
        })
        .collect()
}

/// Structural checks beyond the per-step schema: ids must be unique and every
/// `onFail.retry` must reference an *earlier* step (loops only go backwards). Returns a
/// human-readable problem, or `None` when the list is sound. Mirrors `types.ts::stepsIssue`.
pub fn steps_issue(steps: &[WorkflowStepDef]) -> Option<String> {
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    let dup = ids
        .iter()
        .enumerate()
        .find(|(i, id)| ids.iter().position(|x| x == *id) != Some(*i))
        .map(|(_, id)| *id);
    if let Some(dup) = dup {
        return Some(format!("duplicate step id \"{dup}\""));
    }
    for (i, s) in steps.iter().enumerate() {
        let Some(on_fail) = &s.on_fail else {
            continue;
        };
        let target = ids.iter().position(|id| *id == on_fail.retry.as_str());
        if target.is_none_or(|t| t >= i) {
            return Some(format!(
                "step \"{}\": onFail.retry must reference an earlier step (got \"{}\")",
                s.id, on_fail.retry
            ));
        }
    }
    None
}

pub fn step_kind(step: &WorkflowStepDef) -> StepKind {
    if step.command.is_some() {
        StepKind::Check
    } else {
        StepKind::Agent
    }
}

/// Tools granted to an agent step when its workflow does not provide an explicit list.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &["Read", "Edit", "Write", "Grep", "Glob", "Bash"];

/// Guard later agent sessions against mistaking an earlier chain step's DONE signal for their own.
/// Check steps do not count toward chain position or total.
pub fn chain_step_note(steps: &[WorkflowStepDef], index: usize) -> Option<String> {
    let step = steps.get(index)?;
    if step_kind(step) != StepKind::Agent {
        return None;
    }
    let total = steps
        .iter()
        .filter(|step| step_kind(step) == StepKind::Agent)
        .count();
    if total <= 1 {
        return None;
    }
    let position = steps[..index]
        .iter()
        .filter(|step| step_kind(step) == StepKind::Agent)
        .count()
        + 1;
    let label = step
        .name
        .as_deref()
        .map(|name| format!("\"{name}\""))
        .or_else(|| {
            step.skill
                .as_deref()
                .map(|skill| format!("the \"{skill}\" skill"))
        })
        .unwrap_or_else(|| "this step".to_owned());
    let mut note = format!(
        "This run is a chain of {total} agent steps; you are running step {position} of {total}. Your job in THIS step is {label} — do its work in full. "
    );
    if position > 1 {
        note.push_str(&format!(
            "An earlier step in this same run may already have reported its own work done; that does not mean step {position}'s work is done. "
        ));
    }
    note.push_str(&format!("Only end this turn with DUCK:DONE once step {position}'s own goal is achieved, not just the run's overall task."));
    Some(note)
}

/// The zero-config workflow: one agent step that just does the task. Mirrors
/// `types.ts::QUICK_TASK_WORKFLOW`.
pub fn quick_task_workflow() -> WorkflowDef {
    WorkflowDef {
        name: "quick-task".to_owned(),
        description: Some("One agent run on your task — no ceremony.".to_owned()),
        steps: vec![WorkflowStepDef {
            id: "task".to_owned(),
            name: Some("Do the task".to_owned()),
            prompt: Some("{{task}}".to_owned()),
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        }],
        source: WorkflowSource::BuiltIn,
        path: None,
    }
}

fn validate_step_doc(value: &Value) -> Result<WorkflowStepDef, String> {
    let step: WorkflowStepDef =
        serde_json::from_value(value.clone()).map_err(|e| format!("invalid step: {e}"))?;
    if step.id.is_empty() {
        return Err("a step id must not be empty".to_owned());
    }
    let is_check = step.command.is_some();
    let is_agent = step.prompt.is_some() || step.skill.is_some();
    if is_check == is_agent {
        return Err(
            "a step is either an agent step (prompt/skill) or a check step (command), not both"
                .to_owned(),
        );
    }
    if let Some(on_fail) = &step.on_fail
        && on_fail.max == 0
    {
        return Err("onFail.max must be positive".to_owned());
    }
    Ok(step)
}

/// Validates a raw parsed workflow-file document (`workflowFileSchema`'s `.refine()` rule:
/// `steps` XOR `skills`) and resolves it into `(name, description, steps)` — the
/// `normalizeWorkflowDoc` half of `types.ts`, folded into the same pass since both need the
/// same XOR check. Field-level shape errors (wrong types, an unrecognized `runner`) surface
/// through [`validate_step_doc`]'s `serde_json` error, same as every other schema in this
/// crate that has no `.catch()` to salvage a bad field with.
pub fn parse_workflow_file_doc(
    raw: &Value,
) -> Result<(String, Option<String>, Vec<WorkflowStepDef>), String> {
    let object = raw
        .as_object()
        .ok_or("a workflow file must be a YAML mapping")?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("name is required and must not be empty")?
        .to_owned();
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let steps_present = object.get("steps").is_some_and(|v| !v.is_null());
    let skills_present = object.get("skills").is_some_and(|v| !v.is_null());
    if steps_present == skills_present {
        return Err("a workflow lists either \"steps\" or \"skills\", not both".to_owned());
    }

    let steps = if steps_present {
        let array = object
            .get("steps")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or("steps must be a non-empty array")?;
        array
            .iter()
            .map(validate_step_doc)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let array = object
            .get("skills")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or("skills must be a non-empty array")?;
        let names = array
            .iter()
            .map(|v| {
                let raw = v.as_str().ok_or("skills entries must be strings")?;
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Err("skills entries must not be empty".to_owned())
                } else {
                    Ok(trimmed.to_owned())
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        skills_to_steps(&names)
    };

    Ok((name, description, steps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::workflows::WorkflowOnFail;
    use serde_json::json;

    #[test]
    fn skills_to_steps_dedupes_repeated_skill_names() {
        let steps = skills_to_steps(&["om-a".to_owned(), "om-a".to_owned(), "om-a".to_owned()]);
        let ids: Vec<_> = steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["om-a", "om-a-2", "om-a-3"]);
        assert!(
            steps
                .iter()
                .all(|s| s.prompt.as_deref() == Some("{{task}}"))
        );
    }

    fn step(id: &str) -> WorkflowStepDef {
        WorkflowStepDef {
            id: id.to_owned(),
            name: None,
            prompt: Some("{{task}}".to_owned()),
            skill: None,
            model: None,
            runner: None,
            allowed_tools: None,
            bash_allowlist: None,
            command: None,
            on_fail: None,
        }
    }

    #[test]
    fn steps_issue_flags_duplicate_ids() {
        let steps = vec![step("a"), step("b"), step("a")];
        assert_eq!(
            steps_issue(&steps),
            Some("duplicate step id \"a\"".to_owned())
        );
    }

    #[test]
    fn steps_issue_requires_on_fail_retry_to_target_an_earlier_step() {
        let mut forward = vec![step("a"), step("b")];
        forward[0].on_fail = Some(WorkflowOnFail {
            retry: "b".to_owned(),
            max: 2,
        });
        assert!(steps_issue(&forward).unwrap().contains("earlier step"));

        let mut backward = vec![step("a"), step("b")];
        backward[1].on_fail = Some(WorkflowOnFail {
            retry: "a".to_owned(),
            max: 2,
        });
        assert_eq!(steps_issue(&backward), None);

        let mut missing = vec![step("a")];
        missing[0].on_fail = Some(WorkflowOnFail {
            retry: "nope".to_owned(),
            max: 2,
        });
        assert!(steps_issue(&missing).is_some());
    }

    #[test]
    fn parse_rejects_both_steps_and_skills_present() {
        let doc =
            json!({ "name": "w", "steps": [{"id":"a","prompt":"{{task}}"}], "skills": ["x"] });
        assert!(
            parse_workflow_file_doc(&doc)
                .unwrap_err()
                .contains("either")
        );
    }

    #[test]
    fn parse_rejects_neither_steps_nor_skills_present() {
        let doc = json!({ "name": "w" });
        assert!(
            parse_workflow_file_doc(&doc)
                .unwrap_err()
                .contains("either")
        );
    }

    #[test]
    fn parse_resolves_the_skills_shorthand_into_agent_steps() {
        let doc = json!({ "name": "w", "skills": ["  om-a  ", "om-b"] });
        let (name, description, steps) = parse_workflow_file_doc(&doc).unwrap();
        assert_eq!(name, "w");
        assert_eq!(description, None);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id, "om-a", "the trimmed form is what gets used");
        assert_eq!(steps[0].skill.as_deref(), Some("om-a"));
    }

    #[test]
    fn parse_rejects_a_step_that_is_both_a_check_and_an_agent_step() {
        let doc = json!({
            "name": "w",
            "steps": [{ "id": "a", "prompt": "{{task}}", "command": "echo hi" }],
        });
        assert!(
            parse_workflow_file_doc(&doc)
                .unwrap_err()
                .contains("not both")
        );
    }

    #[test]
    fn parse_rejects_a_step_that_is_neither_a_check_nor_an_agent_step() {
        let doc = json!({ "name": "w", "steps": [{ "id": "a" }] });
        assert!(
            parse_workflow_file_doc(&doc)
                .unwrap_err()
                .contains("not both")
        );
    }

    #[test]
    fn parse_rejects_an_empty_step_id() {
        let doc = json!({ "name": "w", "steps": [{ "id": "", "prompt": "{{task}}" }] });
        assert!(parse_workflow_file_doc(&doc).is_err());
    }

    #[test]
    fn parse_rejects_a_non_positive_on_fail_max() {
        let doc = json!({
            "name": "w",
            "steps": [
                { "id": "a", "command": "echo hi" },
                { "id": "b", "command": "false", "onFail": { "retry": "a", "max": 0 } },
            ],
        });
        assert!(parse_workflow_file_doc(&doc).is_err());
    }

    #[test]
    fn parse_rejects_an_unknown_runner_value_the_way_the_legacy_claude_cli_id_would_be() {
        let doc = json!({
            "name": "w",
            "steps": [{ "id": "a", "prompt": "{{task}}", "runner": "claude-cli" }],
        });
        assert!(parse_workflow_file_doc(&doc).is_err());
    }

    #[test]
    fn chain_notes_count_agent_steps_only_and_default_tools_are_stable() {
        let steps = vec![
            step("first"),
            WorkflowStepDef {
                id: "verify".to_owned(),
                name: None,
                prompt: None,
                skill: None,
                model: None,
                runner: None,
                allowed_tools: None,
                bash_allowlist: None,
                command: Some("true".to_owned()),
                on_fail: None,
            },
            WorkflowStepDef {
                id: "second".to_owned(),
                name: Some("Second pass".to_owned()),
                prompt: Some("{{task}}".to_owned()),
                skill: None,
                model: None,
                runner: None,
                allowed_tools: None,
                bash_allowlist: None,
                command: None,
                on_fail: None,
            },
        ];
        assert!(chain_step_note(&steps, 0).unwrap().contains("step 1 of 2"));
        assert!(chain_step_note(&steps, 1).is_none());
        assert!(chain_step_note(&steps, 2).unwrap().contains("step 2 of 2"));
        assert_eq!(
            DEFAULT_ALLOWED_TOOLS,
            &["Read", "Edit", "Write", "Grep", "Glob", "Bash"]
        );
    }
}
