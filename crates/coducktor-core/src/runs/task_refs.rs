//! PR/issue-number extraction from a task prompt (spec 2026-07-17-task-auto-naming, step 0).
//! Mirrors `packages/cezar/src/runs/task-refs.ts`.
//!
//! The always-available programmatic layer under the LLM namer. Pure — it runs inline at
//! `startRun` and its result both prefixes the heuristic title and cross-checks the namer's
//! structured output (the regex wins every disagreement).

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TaskRefs {
    pub pr_number: Option<i64>,
    pub issue_number: Option<i64>,
    /// A number present in the task whose kind (PR vs issue) is not determinable — a bare
    /// `469` argument or a plain `#469`. Still usable as a title prefix.
    pub ambiguous_number: Option<i64>,
}

pub const MAX_REF: i64 = 10_000_000; // sanity bound — GitHub numbers are far below this

fn num(raw: &str) -> Option<i64> {
    let n: i64 = raw.parse().ok()?;
    (n > 0 && n < MAX_REF).then_some(n)
}

static PR_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)github\.com/[0-9A-Za-z_.-]+/[0-9A-Za-z_.-]+/pull/(\d+)").unwrap()
});
static ISSUE_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)github\.com/[0-9A-Za-z_.-]+/[0-9A-Za-z_.-]+/issues/(\d+)").unwrap()
});
static PR_WORDED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:pull\s+request|pr)\s*#?\s*(\d+)").unwrap());
static ISSUE_WORDED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bissue\s*#?\s*(\d+)").unwrap());
static BARE_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*#?(\d+)\s*$").unwrap());
static HASH_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#(\d+)\b").unwrap());

/// First match wins per kind, scanning the whole prompt.
pub fn extract_task_refs(task: &str) -> TaskRefs {
    let mut refs = TaskRefs::default();

    // 1. Explicit URLs — the strongest signal.
    if let Some(caps) = PR_URL_RE.captures(task) {
        refs.pr_number = num(&caps[1]);
    }
    if let Some(caps) = ISSUE_URL_RE.captures(task) {
        refs.issue_number = num(&caps[1]);
    }

    // 2. Worded references — covers the GitHub-tab templates verbatim ("Address GitHub pull
    //    request #N", "Fix GitHub issue #N") and free text ("pr 437", "PR#437", "review pull
    //    request 437", "issue #12").
    if refs.pr_number.is_none()
        && let Some(caps) = PR_WORDED_RE.captures(task)
    {
        refs.pr_number = num(&caps[1]);
    }
    if refs.issue_number.is_none()
        && let Some(caps) = ISSUE_WORDED_RE.captures(task)
    {
        refs.issue_number = num(&caps[1]);
    }

    // 3. A task that IS a number — the argument-only skill invocation (`469`).
    if refs.pr_number.is_none() && refs.issue_number.is_none() {
        if let Some(caps) = BARE_NUMBER_RE.captures(task) {
            refs.ambiguous_number = num(&caps[1]);
        } else if let Some(caps) = HASH_NUMBER_RE.captures(task) {
            // 4. Last resort: the first `#N` anywhere.
            refs.ambiguous_number = num(&caps[1]);
        }
    }
    refs
}

/// The single number worth prefixing a title with, strongest kind first.
pub fn title_ref_number(refs: &TaskRefs) -> Option<i64> {
    refs.pr_number
        .or(refs.issue_number)
        .or(refs.ambiguous_number)
}

static PR_SKILL_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\W)pr(\W|$)|pull-?request").unwrap());

/// Skill-aware disambiguation for a bare number: `469` handed to a *-review-pr skill is a
/// PR; handed to a *-fix-issue skill it is an issue. Only upgrades `ambiguous_number` —
/// explicit URL/worded matches are never overridden.
pub fn refine_task_refs(refs: TaskRefs, skill_name: Option<&str>) -> TaskRefs {
    let (Some(ambiguous), Some(skill_name)) = (refs.ambiguous_number, skill_name) else {
        return refs;
    };
    let name = skill_name.to_lowercase();
    if PR_SKILL_NAME_RE.is_match(&name) {
        return TaskRefs {
            pr_number: refs.pr_number.or(Some(ambiguous)),
            ambiguous_number: None,
            ..refs
        };
    }
    if name.contains("issue") {
        return TaskRefs {
            issue_number: refs.issue_number.or(Some(ambiguous)),
            ambiguous_number: None,
            ..refs
        };
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(pr: Option<i64>, issue: Option<i64>, ambiguous: Option<i64>) -> TaskRefs {
        TaskRefs {
            pr_number: pr,
            issue_number: issue,
            ambiguous_number: ambiguous,
        }
    }

    #[test]
    fn reads_the_github_tab_templates_verbatim() {
        assert_eq!(
            extract_task_refs(
                "Address GitHub pull request #454: show CI status\n\nhttps://github.com/open-mercato/cezar/pull/454"
            ),
            refs(Some(454), None, None)
        );
        assert_eq!(
            extract_task_refs(
                "Fix GitHub issue #432: bad titles\n\nhttps://github.com/open-mercato/cezar/issues/432"
            ),
            refs(None, Some(432), None)
        );
    }

    #[test]
    fn urls_are_the_strongest_signal_and_set_the_kind() {
        assert_eq!(
            extract_task_refs("see https://github.com/open-mercato/cezar/pull/441 please"),
            refs(Some(441), None, None)
        );
        assert_eq!(
            extract_task_refs("see https://github.com/o-m/repo.name/issues/12"),
            refs(None, Some(12), None)
        );
    }

    #[test]
    fn worded_references_pr_pull_request_issue_with_or_without_hash() {
        assert_eq!(
            extract_task_refs("review pr 437 with autofix"),
            refs(Some(437), None, None)
        );
        assert_eq!(
            extract_task_refs("review PR#437"),
            refs(Some(437), None, None)
        );
        assert_eq!(
            extract_task_refs("continue pull request 468"),
            refs(Some(468), None, None)
        );
        assert_eq!(
            extract_task_refs("triage issue #471 today"),
            refs(None, Some(471), None)
        );
    }

    #[test]
    fn a_bare_number_task_is_ambiguous() {
        assert_eq!(extract_task_refs("469"), refs(None, None, Some(469)));
        assert_eq!(extract_task_refs("  #469  "), refs(None, None, Some(469)));
    }

    #[test]
    fn falls_back_to_the_first_hash_n_anywhere_only_when_nothing_stronger_matched() {
        assert_eq!(
            extract_task_refs("implement the plan from #479 end to end"),
            refs(None, None, Some(479))
        );
        assert_eq!(
            extract_task_refs("fix issue #12 referenced from #479"),
            refs(None, Some(12), None)
        );
    }

    #[test]
    fn finds_nothing_in_plain_prose_and_rejects_absurd_numbers() {
        assert_eq!(
            extract_task_refs("rename the settings page"),
            TaskRefs::default()
        );
        assert_eq!(extract_task_refs("#99999999999"), TaskRefs::default());
    }

    #[test]
    fn a_task_naming_both_a_pr_and_an_issue_keeps_both() {
        assert_eq!(
            extract_task_refs("port the fix from pr 441 onto issue #438"),
            refs(Some(441), Some(438), None)
        );
    }

    #[test]
    fn title_ref_number_prefers_pr_then_issue_then_ambiguous() {
        assert_eq!(title_ref_number(&refs(Some(1), Some(2), Some(3))), Some(1));
        assert_eq!(title_ref_number(&refs(None, Some(2), Some(3))), Some(2));
        assert_eq!(title_ref_number(&refs(None, None, Some(3))), Some(3));
        assert_eq!(title_ref_number(&TaskRefs::default()), None);
    }

    #[test]
    fn refine_classifies_a_bare_number_by_the_skill_it_was_handed_to() {
        assert_eq!(
            refine_task_refs(refs(None, None, Some(469)), Some("om-auto-review-pr")),
            refs(Some(469), None, None)
        );
        assert_eq!(
            refine_task_refs(
                refs(None, None, Some(469)),
                Some("om-auto-continue-pr-loop")
            ),
            refs(Some(469), None, None)
        );
        assert_eq!(
            refine_task_refs(refs(None, None, Some(438)), Some("om-auto-fix-issue")),
            refs(None, Some(438), None)
        );
    }

    #[test]
    fn refine_never_overrides_explicit_refs_and_passes_through_without_a_hint() {
        assert_eq!(
            refine_task_refs(refs(Some(1), None, Some(9)), Some("om-auto-fix-issue")),
            refs(Some(1), Some(9), None)
        );
        assert_eq!(
            refine_task_refs(refs(None, None, Some(9)), None),
            refs(None, None, Some(9))
        );
        assert_eq!(
            refine_task_refs(refs(None, None, Some(9)), Some("om-spec-writing")),
            refs(None, None, Some(9))
        );
    }
}
