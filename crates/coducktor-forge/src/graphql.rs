use std::collections::BTreeMap;

use coducktor_contract::ChecksGlyph;
use serde_json::Value;

use crate::model::{RefStatusBatch, ReferenceKind, ResolvedReference};
use crate::normalize::{
    CheckRun, derive_issue_reference_status, derive_pr_reference_status, first_line,
    rollup_to_checks,
};

pub type GraphqlVariables = BTreeMap<String, String>;

pub fn commit_checks_query(shas: &[String]) -> String {
    let aliases = shas
        .iter()
        .enumerate()
        .map(|(index, sha)| {
            format!(
                "    c{index}: object(oid: \"{sha}\") {{ ... on Commit {{ statusCheckRollup {{ state }} }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join('\n'.encode_utf8(&mut [0; 4]));
    format!(
        "query ($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{\n{aliases}\n  }} }}"
    )
}

pub fn pr_checks_query(numbers: &[u64]) -> String {
    let aliases = numbers
        .iter()
        .enumerate()
        .map(|(index, number)| {
            format!(
                "    p{index}: pullRequest(number: {number}) {{ commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join('\n'.encode_utf8(&mut [0; 4]));
    format!(
        "query ($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{\n{aliases}\n  }} }}"
    )
}

pub fn ref_status_query(numbers: &[u64]) -> String {
    let aliases = numbers
        .iter()
        .enumerate()
        .map(|(index, number)| {
            format!(
                "    r{index}: issueOrPullRequest(number: {number}) {{ __typename ... on PullRequest {{ state isDraft reviewDecision commits(last: 1) {{ nodes {{ commit {{ committedDate statusCheckRollup {{ state }} }} }} }} reviews(last: 1, states: CHANGES_REQUESTED) {{ nodes {{ submittedAt }} }} reviewRequests(first: 1) {{ totalCount }} }} ... on Issue {{ state stateReason }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join('\n'.encode_utf8(&mut [0; 4]));
    format!(
        "query ($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{\n{aliases}\n  }} }}"
    )
}

fn repository(raw: &str) -> Result<serde_json::Map<String, Value>, String> {
    let parsed: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    parsed
        .get("data")
        .and_then(|value| value.get("repository"))
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| "GraphQL response has no repository".to_owned())
}

fn rollup_state(value: Option<&Value>) -> Option<ChecksGlyph> {
    let state = value
        .and_then(|value| value.get("state"))
        .and_then(Value::as_str);
    rollup_to_checks(Some(&[CheckRun {
        state: state.map(str::to_owned),
        status: None,
        conclusion: None,
    }]))
}

pub fn fetch_commit_checks<F>(
    mut run: F,
    owner: &str,
    name: &str,
    shas: &[String],
    chunk_size: usize,
) -> BTreeMap<String, Option<ChecksGlyph>>
where
    F: FnMut(&str, &GraphqlVariables) -> Result<String, String>,
{
    let mut out = BTreeMap::new();
    if shas.is_empty() {
        return out;
    }
    let chunk_size = chunk_size.max(1);
    for chunk in shas.chunks(chunk_size) {
        let variables = BTreeMap::from([
            ("owner".to_owned(), owner.to_owned()),
            ("name".to_owned(), name.to_owned()),
        ]);
        let query = commit_checks_query(chunk);
        let Ok(raw) = run(&query, &variables) else {
            continue;
        };
        let Ok(repository) = repository(&raw) else {
            continue;
        };
        for (index, sha) in chunk.iter().enumerate() {
            let Some(node) = repository.get(&format!("c{index}")) else {
                continue;
            };
            if node.is_null() {
                continue;
            }
            let rollup = node.get("statusCheckRollup");
            out.insert(sha.clone(), rollup_state(rollup));
        }
    }
    out
}

pub fn fetch_pr_checks<F>(
    mut run: F,
    owner: &str,
    name: &str,
    numbers: &[u64],
    chunk_size: usize,
) -> BTreeMap<u64, Option<ChecksGlyph>>
where
    F: FnMut(&str, &GraphqlVariables) -> Result<String, String>,
{
    let mut out = BTreeMap::new();
    if numbers.is_empty() {
        return out;
    }
    let chunk_size = chunk_size.max(1);
    for chunk in numbers.chunks(chunk_size) {
        let variables = BTreeMap::from([
            ("owner".to_owned(), owner.to_owned()),
            ("name".to_owned(), name.to_owned()),
        ]);
        let Ok(raw) = run(&pr_checks_query(chunk), &variables) else {
            continue;
        };
        let Ok(repository) = repository(&raw) else {
            continue;
        };
        for (index, number) in chunk.iter().enumerate() {
            let Some(node) = repository.get(&format!("p{index}")) else {
                continue;
            };
            if node.is_null() {
                continue;
            }
            let rollup = node.pointer("/commits/nodes/0/commit/statusCheckRollup");
            out.insert(*number, rollup_state(rollup));
        }
    }
    out
}

fn optional_string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

pub fn fetch_ref_statuses<F>(
    mut run: F,
    owner: &str,
    name: &str,
    numbers: &[u64],
    chunk_size: usize,
) -> RefStatusBatch
where
    F: FnMut(&str, &GraphqlVariables) -> Result<String, String>,
{
    let mut out = RefStatusBatch::default();
    if numbers.is_empty() {
        return out;
    }
    let chunk_size = chunk_size.max(1);
    for chunk in numbers.chunks(chunk_size) {
        let variables = BTreeMap::from([
            ("owner".to_owned(), owner.to_owned()),
            ("name".to_owned(), name.to_owned()),
        ]);
        let query = ref_status_query(chunk);
        let raw = match run(&query, &variables) {
            Ok(raw) => raw,
            Err(error) => {
                out.failed.extend_from_slice(chunk);
                if out.reason.is_none() {
                    out.reason = Some(first_line(&error));
                }
                continue;
            }
        };
        let repository = match repository(&raw) {
            Ok(repository) => repository,
            Err(error) => {
                out.failed.extend_from_slice(chunk);
                if out.reason.is_none() {
                    out.reason = Some(first_line(&error));
                }
                continue;
            }
        };
        for (index, number) in chunk.iter().enumerate() {
            let Some(node) = repository.get(&format!("r{index}")) else {
                continue;
            };
            if node.is_null() {
                continue;
            }
            let Some(kind) = optional_string(node.get("__typename")) else {
                continue;
            };
            if kind == "PullRequest" {
                let head = node.pointer("/commits/nodes/0/commit");
                let checks = rollup_state(head.and_then(|head| head.get("statusCheckRollup")));
                let status = derive_pr_reference_status(
                    optional_string(node.get("state")).unwrap_or_default(),
                    node.get("isDraft")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    optional_string(node.get("reviewDecision")),
                    checks,
                    optional_string(head.and_then(|head| head.get("committedDate"))),
                    optional_string(node.pointer("/reviews/nodes/0/submittedAt")),
                    node.pointer("/reviewRequests/totalCount")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        > 0,
                );
                out.resolved.insert(
                    *number,
                    ResolvedReference {
                        kind: ReferenceKind::Pr,
                        status,
                    },
                );
            } else if kind == "Issue" {
                let status = derive_issue_reference_status(
                    optional_string(node.get("state")).unwrap_or_default(),
                    optional_string(node.get("stateReason")),
                );
                out.resolved.insert(
                    *number,
                    ResolvedReference {
                        kind: ReferenceKind::Issue,
                        status,
                    },
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(repository: Value) -> String {
        serde_json::json!({"data":{"repository":repository}}).to_string()
    }

    #[test]
    fn commit_and_pr_queries_are_bounded_and_aliasable() {
        let shas = vec!["a".repeat(40), "b".repeat(40)];
        let query = commit_checks_query(&shas);
        assert!(query.contains("c0: object(oid:"));
        assert!(query.contains(&shas[1]));
        assert!(pr_checks_query(&[7, 8]).contains("p1: pullRequest(number: 8)"));
    }

    #[test]
    fn graphql_failures_are_scoped_to_their_chunk() {
        let mut calls = 0;
        let out = fetch_ref_statuses(
            |query, _| {
                calls += 1;
                if query.contains("number: 3") {
                    Err("HTTP 502".to_owned())
                } else {
                    Ok(reply(serde_json::json!({
                        "r0": {"__typename":"PullRequest", "state":"OPEN", "isDraft":false, "reviewDecision":null, "commits":{"nodes":[]}},
                        "r1": {"__typename":"PullRequest", "state":"OPEN", "isDraft":false, "reviewDecision":null, "commits":{"nodes":[]}}
                    })))
                }
            },
            "o",
            "n",
            &[1, 2, 3, 4],
            2,
        );
        assert_eq!(calls, 2);
        assert!(out.resolved.contains_key(&1));
        assert_eq!(out.failed, vec![3, 4]);
    }
}
