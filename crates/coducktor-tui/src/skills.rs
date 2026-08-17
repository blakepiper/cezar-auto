//! Shared skill presentation rules: project-first ordering, typed-query filtering and ranking,
//! and promotion of the "Most used" tier. Every function here is pure and covered by focused
//! tests.

use std::collections::BTreeMap;

use coducktor_contract::{Skill, SkillSource, WorkflowDef};

/// Project-oriented skills first, user-global after.
/// Team skills are configured and cached per project even though their files live
/// in a shared remote repo, so they belong with project skills. Only `global`
/// comes from the user's home catalog.
pub fn is_project_skill(source: SkillSource) -> bool {
    matches!(
        source,
        SkillSource::Ai | SkillSource::Legacy | SkillSource::Agents | SkillSource::Team
    )
}

/// How many "Most used" skills a picker promotes above the locality groups.
pub const MOST_USED_LIMIT: usize = 5;

/// The three display tiers every skill picker renders, in this order.
#[derive(Debug, Clone, Default)]
pub struct SkillTiers<'a> {
    /// Skills actually picked before (`skillUsage` count > 0), frequency descending, capped.
    pub most_used: Vec<&'a Skill>,
    /// Remaining project skills, in the incoming alphabetical order.
    pub project: Vec<&'a Skill>,
    /// Remaining user-global skills, in the incoming order.
    pub global: Vec<&'a Skill>,
}

/// One skill's count out of the `skillUsage` map. A plain map lookup is safe here
/// (unlike the JS Object.prototype trap) because `BTreeMap` has no inherited keys.
fn usage_count(usage: Option<&BTreeMap<String, f64>>, name: &str) -> f64 {
    usage.and_then(|map| map.get(name)).copied().unwrap_or(0.0)
}

/// A pure reducer over the ui-state `skillUsage` map: bump one skill's count
/// by one. The UI-state merge is shallow, so a successful run start always sends the whole
/// updated map back, never just the one changed entry.
pub fn bump_skill_usage(
    usage: Option<&BTreeMap<String, f64>>,
    name: &str,
) -> BTreeMap<String, f64> {
    let mut next = usage.cloned().unwrap_or_default();
    *next.entry(name.to_owned()).or_default() += 1.0;
    next
}

/// Does `query` fuzzy-match `candidate`? Case-insensitive subsequence — `omfx`
/// finds `om-fix-issue`, preserving the incoming order for matching candidates.
pub fn fuzzy_match(candidate: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack: Vec<char> = candidate.to_lowercase().chars().collect();
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    let mut at = 0;
    for char in needle {
        let found = haystack[at..]
            .iter()
            .position(|candidate_char| *candidate_char == char);
        match found {
            Some(offset) => at += offset + 1,
            None => return false,
        }
    }
    true
}

/// Characters that begin a new "word" inside a skill value. Matches the JS
/// `WORD_BOUNDARY = /[\s\-/_.]/` for the ASCII whitespace every skill name uses.
fn is_word_boundary(character: char) -> bool {
    matches!(
        character,
        ' ' | '\t' | '\n' | '\r' | '\u{000b}' | '\u{000c}' | '\u{00a0}' | '-' | '/' | '_' | '.'
    )
}

/// How well a whole `query` matches a single `text` (a skill name or its description).
/// 0 = no match; higher = better: exact > prefix > word-boundary hit > buried substring >
/// subsequence.
pub fn match_score(text: &str, query: &str) -> u8 {
    if query.is_empty() {
        return 1;
    }
    let haystack = text.to_lowercase();
    let needle = query.to_lowercase();
    if haystack == needle {
        return 6;
    }
    if haystack.starts_with(&needle) {
        return 5;
    }
    if let Some(index) = haystack.find(&needle)
        && index > 0
    {
        let before = haystack.chars().nth(index - 1);
        return match before {
            Some(character) if is_word_boundary(character) => 4,
            _ => 3,
        };
    }
    if fuzzy_match(&haystack, &needle) {
        1
    } else {
        0
    }
}

/// A name hit outranks a description-only hit by this much, so an (almost-)exact
/// name match always sorts above a skill that merely mentions the query in its
/// description.
const NAME_MATCH_BONUS: f64 = 10.0;

/// How well a name/description pair matches a typed query. The query is split on
/// whitespace and EVERY word must appear in the name or the description; a query
/// that misses any word scores 0. Each word contributes its `match_score` quality,
/// and a word that lands in the NAME is boosted over one that only lands in the
/// description. Empty query is a neutral match (1).
pub fn query_score(name: &str, description: Option<&str>, query: &str) -> f64 {
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if words.is_empty() {
        return 1.0;
    }
    let mut total = 0.0;
    for word in words {
        let name_score = match_score(name, &word);
        let desc_score = description
            .map(|desc| match_score(desc, &word))
            .unwrap_or(0);
        if name_score == 0 && desc_score == 0 {
            return 0.0; // every word must match somewhere
        }
        total += if name_score > 0 {
            f64::from(name_score) + NAME_MATCH_BONUS
        } else {
            f64::from(desc_score)
        };
    }
    total
}

/// A heavily-used skill may win among comparably-scoring matches, but never
/// outrank a clearly better name match: the bonus is bounded to `MOST_USED_LIMIT *
/// USAGE_BONUS_STEP`, well under the `NAME_MATCH_BONUS` gap between match tiers.
const USAGE_BONUS_STEP: f64 = 0.5;

/// The minimal "has a name and optional description" surface the shared ranking
/// engine needs — implemented for both `Skill` and `WorkflowDef`.
trait Matchable {
    fn name(&self) -> &str;
    fn description(&self) -> Option<&str>;
}

impl Matchable for Skill {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

impl Matchable for WorkflowDef {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

impl<T: Matchable> Matchable for &T {
    fn name(&self) -> &str {
        (*self).name()
    }
    fn description(&self) -> Option<&str> {
        (*self).description()
    }
}

/// Filter a name/description list down to matches and rank them by match quality
/// preserving the incoming order for an empty query and for equally-scored ties. When `usage`
/// is given, a bounded usage bonus is folded into the
/// score and the usage count breaks remaining ties, so a typed query no longer
/// discards frequency entirely.
fn rank_by_query<'a, T>(
    items: &'a [T],
    query: &str,
    usage: Option<&BTreeMap<String, f64>>,
) -> Vec<&'a T>
where
    T: Matchable,
{
    if query.trim().is_empty() {
        return items.iter().collect();
    }
    let mut scored: Vec<(f64, f64, usize, &'a T)> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let base = query_score(item.name(), item.description(), query);
            let count = usage_count(usage, item.name());
            let bonus = (count.min(MOST_USED_LIMIT as f64)) * USAGE_BONUS_STEP;
            let score = if base > 0.0 { base + bonus } else { 0.0 };
            (score, count, index, item)
        })
        .filter(|(score, ..)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| b.1.total_cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    scored.into_iter().map(|(_, _, _, item)| item).collect()
}

/// Rank skills for a grouped picker by match quality, keeping the caller's incoming
/// order for ties and the empty query. Callers split the result into their own display groups.
pub fn search_skills<'a>(
    skills: &'a [Skill],
    query: &str,
    usage: Option<&BTreeMap<String, f64>>,
) -> Vec<&'a Skill> {
    rank_by_query(skills, query, usage)
}

/// Rank workflows for a picker by match quality — the workflow counterpart of `search_skills`.
pub fn search_workflows<'a>(workflows: &'a [WorkflowDef], query: &str) -> Vec<&'a WorkflowDef> {
    rank_by_query(workflows, query, None)
}

/// Partition skills into the picker display tiers: **Most used** first —
/// skills with a `skillUsage` count > 0, frequency descending, ties broken
/// locality-first then by incoming order, capped at `most_used_limit` — then
/// **project**, then **global**, each remainder keeping the incoming order. A skill
/// promoted into `most_used` is NOT repeated in its locality group. With no usage
/// at all this degrades to the plain project-first split.
pub fn partition_skills_for_display<'a>(
    skills: &'a [Skill],
    usage: Option<&BTreeMap<String, f64>>,
    most_used_limit: usize,
) -> SkillTiers<'a> {
    partition_skill_refs(&skills.iter().collect::<Vec<_>>(), usage, most_used_limit)
}

/// The borrowed-slice counterpart: partitions an already-filtered/ranked list of
/// `&Skill` (e.g. a `search_skills` result) into the same tiers, using the input
/// order for ties.
pub fn partition_skill_refs<'a>(
    skills: &[&'a Skill],
    usage: Option<&BTreeMap<String, f64>>,
    most_used_limit: usize,
) -> SkillTiers<'a> {
    let mut candidates: Vec<(f64, bool, usize, &'a Skill)> = skills
        .iter()
        .enumerate()
        .map(|(index, skill)| {
            (
                usage_count(usage, &skill.name),
                is_project_skill(skill.source),
                index,
                *skill,
            )
        })
        .filter(|(count, _, _, _)| *count > 0.0)
        .collect();
    candidates.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| (b.1 as u8).cmp(&(a.1 as u8)))
            .then_with(|| a.2.cmp(&b.2))
    });
    let most_used: Vec<&'a Skill> = candidates
        .iter()
        .take(most_used_limit)
        .map(|(_, _, _, skill)| *skill)
        .collect();
    let promoted: std::collections::HashSet<usize> = candidates
        .iter()
        .take(most_used_limit)
        .map(|(_, _, index, _)| *index)
        .collect();
    let rest = skills
        .iter()
        .enumerate()
        .filter(|(index, _)| !promoted.contains(index))
        .map(|(_, skill)| *skill);
    SkillTiers {
        most_used,
        project: rest
            .clone()
            .filter(|skill| is_project_skill(skill.source))
            .collect(),
        global: rest
            .filter(|skill| !is_project_skill(skill.source))
            .collect(),
    }
}

/// The flat frequency order: the display tiers flattened —
/// most-used first (frequency descending, capped), then project, then global.
pub fn order_skills_by_usage<'a>(
    skills: &'a [Skill],
    usage: Option<&BTreeMap<String, f64>>,
) -> Vec<&'a Skill> {
    let tiers = partition_skills_for_display(skills, usage, MOST_USED_LIMIT);
    [tiers.most_used, tiers.project, tiers.global].concat()
}

/// The `/` autocomplete's list for a typed query: ordered most-used → project →
/// global, then filtered and **ranked by match quality**. Without `usage` this is the
/// project-first behavior.
pub fn filter_skills<'a>(
    skills: &'a [Skill],
    query: &str,
    usage: Option<&BTreeMap<String, f64>>,
) -> Vec<&'a Skill> {
    let ordered = order_skills_by_usage(skills, usage);
    rank_by_query(&ordered, query, usage)
        .into_iter()
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, source: SkillSource, description: Option<&str>) -> Skill {
        Skill {
            name: name.to_owned(),
            description: description.map(ToOwned::to_owned),
            interactive: None,
            body: String::new(),
            path: format!("/skills/{name}.md"),
            source,
            team: None,
        }
    }

    fn workflow(
        name: &str,
        description: Option<&str>,
        source: coducktor_contract::WorkflowSource,
    ) -> WorkflowDef {
        WorkflowDef {
            name: name.to_owned(),
            description: description.map(ToOwned::to_owned),
            steps: Vec::new(),
            source,
            path: None,
        }
    }

    fn usage(entries: &[(&str, f64)]) -> BTreeMap<String, f64> {
        entries
            .iter()
            .map(|(name, count)| ((*name).to_owned(), *count))
            .collect()
    }

    #[test]
    fn classifies_every_skill_source_value() {
        assert!(is_project_skill(SkillSource::Ai));
        assert!(is_project_skill(SkillSource::Legacy));
        assert!(is_project_skill(SkillSource::Agents));
        assert!(!is_project_skill(SkillSource::Global));
        assert!(is_project_skill(SkillSource::Team));
    }

    #[test]
    fn promotes_used_skills_above_locality() {
        let skills = vec![
            s("g1", SkillSource::Global, None),
            s("p1", SkillSource::Agents, None),
            s("g2", SkillSource::Global, None),
            s("p2", SkillSource::Ai, None),
        ];
        let ordered = order_skills_by_usage(
            &skills,
            Some(&usage(&[("g2", 5.0), ("p2", 9.0), ("p1", 1.0)])),
        );
        assert_eq!(names(&ordered), ["p2", "g2", "p1", "g1"]);
    }

    #[test]
    fn splits_into_the_three_tiers_without_repeating_a_promoted_skill() {
        let skills = vec![
            s("g1", SkillSource::Global, None),
            s("p1", SkillSource::Agents, None),
            s("g2", SkillSource::Global, None),
            s("p2", SkillSource::Ai, None),
        ];
        let tiers = partition_skills_for_display(
            &skills,
            Some(&usage(&[("g2", 5.0), ("p1", 1.0)])),
            MOST_USED_LIMIT,
        );
        assert_eq!(names(&tiers.most_used), ["g2", "p1"]);
        assert_eq!(names(&tiers.project), ["p2"]);
        assert_eq!(names(&tiers.global), ["g1"]);
    }

    #[test]
    fn caps_most_used_at_the_limit_and_overflow_rejoins_its_locality_group() {
        let skills = vec![
            s("g1", SkillSource::Global, None),
            s("p1", SkillSource::Agents, None),
            s("g2", SkillSource::Global, None),
            s("p2", SkillSource::Ai, None),
        ];
        let tiers = partition_skills_for_display(
            &skills,
            Some(&usage(&[
                ("g1", 9.0),
                ("p1", 8.0),
                ("g2", 7.0),
                ("p2", 6.0),
            ])),
            2,
        );
        assert_eq!(names(&tiers.most_used), ["g1", "p1"]);
        assert_eq!(names(&tiers.project), ["p2"]);
        assert_eq!(names(&tiers.global), ["g2"]);
    }

    #[test]
    fn zero_usage_degrades_to_the_plain_project_first_split() {
        let skills = vec![
            s("g1", SkillSource::Global, None),
            s("p1", SkillSource::Agents, None),
            s("g2", SkillSource::Global, None),
            s("p2", SkillSource::Ai, None),
        ];
        let tiers = partition_skills_for_display(&skills, None, MOST_USED_LIMIT);
        assert!(tiers.most_used.is_empty());
        assert_eq!(names(&tiers.project), ["p1", "p2"]);
        assert_eq!(names(&tiers.global), ["g1", "g2"]);
        assert_eq!(
            names(&order_skills_by_usage(&skills, None)),
            ["p1", "p2", "g1", "g2"]
        );
    }

    #[test]
    fn equal_counts_inside_most_used_break_locality_first_then_input_order() {
        let skills = vec![
            s("g1", SkillSource::Global, None),
            s("p1", SkillSource::Agents, None),
            s("g2", SkillSource::Global, None),
            s("p2", SkillSource::Ai, None),
        ];
        let ordered = order_skills_by_usage(
            &skills,
            Some(&usage(&[
                ("g1", 3.0),
                ("p1", 3.0),
                ("g2", 3.0),
                ("p2", 3.0),
            ])),
        );
        assert_eq!(names(&ordered), ["p1", "p2", "g1", "g2"]);
    }

    #[test]
    fn bump_skill_usage_starts_fresh_and_increments() {
        assert_eq!(bump_skill_usage(None, "om-fix"), usage(&[("om-fix", 1.0)]));
        assert_eq!(
            bump_skill_usage(
                Some(&usage(&[("om-fix", 2.0), ("om-review", 7.0)])),
                "om-fix"
            ),
            usage(&[("om-fix", 3.0), ("om-review", 7.0)])
        );
        assert_eq!(
            bump_skill_usage(Some(&usage(&[("om-fix", 1.0)])), "om-review"),
            usage(&[("om-fix", 1.0), ("om-review", 1.0)])
        );
    }

    #[test]
    fn fuzzy_match_handles_common_skill_queries() {
        let table: &[(&str, &str, bool)] = &[
            ("om-fix-issue", "", true),
            ("om-fix-issue", "fix", true),
            ("om-fix-issue", "omfx", true),
            ("om-fix-issue", "OMFX", true),
            ("om-fix-issue", "xz", false),
            ("om-fix-issue", "issuefix", false),
            ("src/engine/state.rs", "srs", true),
        ];
        for (candidate, query, hit) in table {
            assert_eq!(
                fuzzy_match(candidate, query),
                *hit,
                "query {query:?} against {candidate:?}"
            );
        }
    }

    #[test]
    fn filter_skills_narrows_without_reordering_across_the_project_global_split() {
        let skills = vec![
            s(
                "global-deploy",
                SkillSource::Global,
                Some("Deploy from anywhere"),
            ),
            s("project-deploy", SkillSource::Ai, None),
            s(
                "project-review",
                SkillSource::Legacy,
                Some("Review the diff"),
            ),
        ];
        assert_eq!(
            names(&filter_skills(&skills, "", None)),
            ["project-deploy", "project-review", "global-deploy"]
        );
        assert_eq!(
            names(&filter_skills(&skills, "deploy", None)),
            ["project-deploy", "global-deploy"]
        );
        assert_eq!(
            names(&filter_skills(&skills, "anywhere", None)),
            ["global-deploy"]
        );
        assert!(filter_skills(&skills, "zzz", None).is_empty());
    }

    #[test]
    fn match_score_ranks_a_stronger_match_higher() {
        assert!(match_score("review", "review") > match_score("review-prs", "review"));
        assert!(match_score("review-prs", "review") > match_score("om-code-review", "review"));
        assert!(match_score("om-code-review", "review") > match_score("previewer", "review"));
        assert!(match_score("previewer", "review") > match_score("om-fix-issue", "omfx"));
        assert_eq!(match_score("om-fix-issue", "zzz"), 0);
        assert_eq!(match_score("anything", ""), 1);
    }

    #[test]
    fn filter_skills_ranks_an_exact_name_match_above_a_partial_one() {
        let skills = vec![
            s("om-code-review", SkillSource::Ai, None),
            s("review", SkillSource::Ai, None),
        ];
        assert_eq!(
            names(&filter_skills(&skills, "review", None)),
            ["review", "om-code-review"]
        );
    }

    #[test]
    fn filter_skills_ranks_a_prefix_match_above_a_word_boundary_match() {
        let skills = vec![
            s("om-auto-deploy", SkillSource::Ai, None),
            s("deploy-app", SkillSource::Ai, None),
        ];
        assert_eq!(
            names(&filter_skills(&skills, "deploy", None)),
            ["deploy-app", "om-auto-deploy"]
        );
    }

    #[test]
    fn filter_skills_keeps_project_first_order_when_matches_are_equally_good() {
        let skills = vec![
            s("global-review", SkillSource::Global, None),
            s("project-review", SkillSource::Ai, None),
        ];
        assert_eq!(
            names(&filter_skills(&skills, "review", None)),
            ["project-review", "global-review"]
        );
    }

    #[test]
    fn search_skills_ranks_the_almost_exact_name_match_first() {
        let skills = vec![
            s(
                "om-auto-fix-issue",
                SkillSource::Ai,
                Some("Fix an issue; runs om-fix internally"),
            ),
            s("om-fix", SkillSource::Ai, Some("Apply the minimal fix")),
            s("om-open-pr", SkillSource::Global, Some("Open a PR")),
        ];
        assert_eq!(
            names(&search_skills(&skills, "om-fix", None)),
            ["om-fix", "om-auto-fix-issue"]
        );
        assert_eq!(
            names(&search_skills(&skills, "", None)),
            ["om-auto-fix-issue", "om-fix", "om-open-pr"]
        );
        assert!(search_skills(&skills, "zzz", None).is_empty());
    }

    #[test]
    fn search_skills_name_match_outranks_description_only() {
        let skills = vec![
            s(
                "om-open-pr",
                SkillSource::Ai,
                Some("Open a PR for an issue"),
            ),
            s("om-auto-fix-issue", SkillSource::Ai, None),
        ];
        assert_eq!(
            names(&search_skills(&skills, "issue", None)),
            ["om-auto-fix-issue", "om-open-pr"]
        );
    }

    #[test]
    fn search_workflows_ranks_by_match_quality() {
        let workflows = vec![
            workflow(
                "ship-it",
                Some("review then deploy"),
                coducktor_contract::WorkflowSource::File,
            ),
            workflow("review", None, coducktor_contract::WorkflowSource::File),
        ];
        assert_eq!(
            search_workflows(&workflows, "review")
                .into_iter()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>(),
            ["review", "ship-it"]
        );
    }

    #[test]
    fn query_score_prefers_a_name_hit_over_description_only() {
        assert!(
            query_score("deploy", Some("unrelated"), "deploy")
                > query_score("other", Some("deploy tool"), "deploy")
        );
        assert_eq!(query_score("alpha", Some("beta"), "zzz"), 0.0);
    }

    #[test]
    fn usage_folds_into_query_ranking_and_the_autocomplete_order() {
        let skills = vec![
            s("project-deploy", SkillSource::Ai, None),
            s(
                "global-deploy",
                SkillSource::Global,
                Some("Deploy from anywhere"),
            ),
        ];
        assert_eq!(
            names(&filter_skills(
                &skills,
                "",
                Some(&usage(&[("global-deploy", 3.0)]))
            )),
            ["global-deploy", "project-deploy"]
        );
        assert_eq!(
            names(&filter_skills(&skills, "", None)),
            ["project-deploy", "global-deploy"]
        );
        assert_eq!(
            names(&filter_skills(
                &skills,
                "deploy",
                Some(&usage(&[("global-deploy", 3.0)]))
            )),
            ["global-deploy", "project-deploy"]
        );
        assert_eq!(
            names(&search_skills(
                &skills,
                "deploy",
                Some(&usage(&[("project-deploy", 2.0)]))
            )),
            ["project-deploy", "global-deploy"]
        );
    }

    #[test]
    fn the_usage_bonus_is_bounded_never_outranking_a_better_name_match() {
        let skills = vec![
            s("om-fix", SkillSource::Ai, None),
            s(
                "om-auto-fix-issue",
                SkillSource::Ai,
                Some("runs om-fix internally"),
            ),
        ];
        assert_eq!(
            names(&search_skills(
                &skills,
                "om-fix",
                Some(&usage(&[("om-auto-fix-issue", 999.0)]))
            )),
            ["om-fix", "om-auto-fix-issue"]
        );
    }

    fn names(items: &[&Skill]) -> Vec<String> {
        items.iter().map(|skill| skill.name.clone()).collect()
    }
}
