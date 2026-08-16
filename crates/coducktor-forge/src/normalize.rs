use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use coducktor_contract::{
    ChecksGlyph, GithubComment, GithubCommentKind, GithubReviewState, GithubTimelineEvent,
    GithubTimelineEventKind, GithubTimelineLabel, ReferenceStatus,
};
use serde::Deserialize;
use serde_json::Value;

use crate::model::ResolvedReference;

pub const GH_COUNTS_MAX_PAGES: usize = 10;
pub const GH_MAX_LIMIT: usize = 1_000;
pub const GH_CHECKS_MAX: usize = 100;
pub const GH_REF_STATUS_MAX: usize = 100;
pub const THREAD_ENTRY_CAP: usize = 200;
pub const TIMELINE_EVENT_CAP: usize = 200;
pub const TIMELINE_MAX_PAGES: usize = 10;
pub const TIMELINE_BUDGET_MS: u64 = 15_000;
pub const TIMELINE_MIN_PAGE_MS: u64 = 2_000;
pub const COMMIT_CHECKS_CHUNK: usize = 50;
pub const COMMENT_BODY_CAP: usize = 8_000;
pub const GH_PR_DIFF_FILE_CAP: usize = 300;
pub const GH_PR_PATCH_CAP: usize = 512 * 1024;
pub const GH_PR_DIFF_JSON_CAP: usize = 4 * 1024 * 1024;
pub const CACHE_MS: u64 = 60_000;
pub const REF_STATUS_CLOSED_TTL: u64 = 10 * 60_000;
pub const REF_STATUS_MERGED_TTL: u64 = 24 * 60 * 60_000;
pub const REF_STATUS_RETRY_MS: u64 = 5 * 60_000;

pub fn fetch_pr_file_pages<F>(mut run_page: F) -> Result<Vec<Value>, String>
where
    F: FnMut(usize) -> Result<String, String>,
{
    let mut rows = Vec::new();
    for page in 1..=GH_PR_DIFF_FILE_CAP / 100 {
        let raw = run_page(page)?;
        let parsed: Vec<Value> = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let short = parsed.len() < 100;
        rows.extend(parsed);
        if short {
            break;
        }
    }
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub state: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
}

pub fn rollup_to_checks(rollup: Option<&[CheckRun]>) -> Option<ChecksGlyph> {
    let rollup = rollup?;
    if rollup.is_empty() {
        return None;
    }
    let states = rollup.iter().map(|run| {
        run.conclusion
            .as_deref()
            .or(run.state.as_deref())
            .or(run.status.as_deref())
            .unwrap_or("")
            .to_ascii_uppercase()
    });
    let states: Vec<String> = states.collect();
    if states.iter().any(|state| {
        matches!(
            state.as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED"
        )
    }) {
        return Some(ChecksGlyph::Failing);
    }
    if states.iter().any(|state| {
        matches!(
            state.as_str(),
            "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | ""
        )
    }) {
        return Some(ChecksGlyph::Pending);
    }
    Some(ChecksGlyph::Passing)
}

pub fn parse_owner_name(value: &str) -> Option<(String, String)> {
    let mut pieces = value.trim().split('/');
    let owner = pieces.next()?.to_owned();
    let name = pieces.next()?.to_owned();
    if owner.is_empty() || name.is_empty() || pieces.next().is_some() {
        return None;
    }
    Some((owner, name))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountsPage {
    pub counts: BTreeMap<u64, u64>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

pub fn parse_counts_page(raw: &str, root: &str) -> Result<CountsPage, String> {
    let value: Value = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let page = value
        .get("data")
        .and_then(|v| v.get("repository"))
        .and_then(|v| v.get(root))
        .ok_or_else(|| "missing GraphQL counts page".to_owned())?;
    let nodes = page
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "counts nodes is not an array".to_owned())?;
    let mut counts = BTreeMap::new();
    for node in nodes {
        let number = node
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| "count node has no number".to_owned())?;
        let count = node
            .get("comments")
            .and_then(|v| v.get("totalCount"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "count node has no totalCount".to_owned())?;
        counts.insert(number, count);
    }
    let page_info = page
        .get("pageInfo")
        .ok_or_else(|| "counts page has no pageInfo".to_owned())?;
    Ok(CountsPage {
        counts,
        has_next_page: page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        end_cursor: page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn counts_query(root: &str) -> String {
    format!(
        "query ($owner: String!, $name: String!, $endCursor: String) {{ repository(owner: $owner, name: $name) {{ {root}(first: 100, after: $endCursor, states: OPEN, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ number comments {{ totalCount }} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
    )
}

pub fn fetch_comment_counts<F>(
    mut run: F,
    owner: &str,
    name: &str,
    max_pages: usize,
) -> (BTreeMap<u64, u64>, BTreeMap<u64, u64>)
where
    F: FnMut(&str, &BTreeMap<String, String>) -> Result<String, String>,
{
    fn one<F>(
        run: &mut F,
        owner: &str,
        name: &str,
        root: &str,
        max_pages: usize,
    ) -> Result<BTreeMap<u64, u64>, String>
    where
        F: FnMut(&str, &BTreeMap<String, String>) -> Result<String, String>,
    {
        let mut cursor: Option<String> = None;
        let mut out = BTreeMap::new();
        for _ in 0..max_pages {
            let mut vars = BTreeMap::from([
                ("owner".to_owned(), owner.to_owned()),
                ("name".to_owned(), name.to_owned()),
            ]);
            if let Some(cursor) = &cursor {
                vars.insert("endCursor".to_owned(), cursor.clone());
            }
            let page = parse_counts_page(&run(&counts_query(root), &vars)?, root)?;
            out.extend(page.counts);
            if !page.has_next_page || page.end_cursor.is_none() {
                break;
            }
            cursor = page.end_cursor;
        }
        Ok(out)
    }

    let issues = one(&mut run, owner, name, "issues", max_pages).unwrap_or_default();
    let prs = one(&mut run, owner, name, "pullRequests", max_pages).unwrap_or_default();
    (issues, prs)
}

#[derive(Debug, Deserialize)]
struct RawUser {
    login: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawComment {
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
}

pub fn cap_body(value: Option<&str>) -> String {
    value
        .unwrap_or_default()
        .chars()
        .take(COMMENT_BODY_CAP)
        .collect()
}

pub fn normalize_comments(raw: &Value) -> Result<Vec<GithubComment>, String> {
    let rows: Vec<RawComment> =
        serde_json::from_value(raw.clone()).map_err(|err| err.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| GithubComment {
            id: row.id,
            author: row
                .user
                .as_ref()
                .map_or_else(|| "?".to_owned(), |u| u.login.clone()),
            avatar_url: row.user.as_ref().and_then(|u| u.avatar_url.clone()),
            created_at: row.created_at,
            body: cap_body(row.body.as_deref()),
            kind: GithubCommentKind::Comment,
            review_state: None,
            url: row.html_url,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct RawReview {
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    html_url: String,
}

pub fn normalize_reviews(raw: &Value) -> Result<Vec<GithubComment>, String> {
    let rows: Vec<RawReview> =
        serde_json::from_value(raw.clone()).map_err(|err| err.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let state = row.state.to_ascii_uppercase();
            if row.body.as_deref().unwrap_or_default().trim().is_empty()
                && matches!(state.as_str(), "COMMENTED" | "PENDING")
            {
                return None;
            }
            let review_state = match state.as_str() {
                "APPROVED" => Some(GithubReviewState::Approved),
                "CHANGES_REQUESTED" => Some(GithubReviewState::ChangesRequested),
                "COMMENTED" => Some(GithubReviewState::Commented),
                "DISMISSED" => Some(GithubReviewState::Dismissed),
                _ => None,
            };
            Some(GithubComment {
                id: row.id,
                author: row
                    .user
                    .as_ref()
                    .map_or_else(|| "?".to_owned(), |u| u.login.clone()),
                avatar_url: row.user.as_ref().and_then(|u| u.avatar_url.clone()),
                created_at: row.submitted_at.unwrap_or_default(),
                body: cap_body(row.body.as_deref()),
                kind: GithubCommentKind::Review,
                review_state,
                url: row.html_url,
            })
        })
        .collect())
}

pub fn merge_thread(mut parts: Vec<Vec<GithubComment>>, cap: usize) -> (Vec<GithubComment>, bool) {
    let mut all = Vec::new();
    for part in parts.drain(..) {
        all.extend(part);
    }
    all.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let truncated = all.len() > cap;
    if truncated {
        all.truncate(cap);
    }
    (all, truncated)
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn parse_iso(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value).ok().map(|date| {
        date.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

pub const TIMELINE_EVENT_KINDS: &[GithubTimelineEventKind] = &[
    GithubTimelineEventKind::Committed,
    GithubTimelineEventKind::Labeled,
    GithubTimelineEventKind::Unlabeled,
    GithubTimelineEventKind::Assigned,
    GithubTimelineEventKind::Unassigned,
    GithubTimelineEventKind::Merged,
    GithubTimelineEventKind::Closed,
    GithubTimelineEventKind::Reopened,
    GithubTimelineEventKind::HeadRefForcePushed,
    GithubTimelineEventKind::CrossReferenced,
    GithubTimelineEventKind::Renamed,
];

fn timeline_kind(value: &str) -> Option<GithubTimelineEventKind> {
    Some(match value {
        "committed" => GithubTimelineEventKind::Committed,
        "labeled" => GithubTimelineEventKind::Labeled,
        "unlabeled" => GithubTimelineEventKind::Unlabeled,
        "assigned" => GithubTimelineEventKind::Assigned,
        "unassigned" => GithubTimelineEventKind::Unassigned,
        "merged" => GithubTimelineEventKind::Merged,
        "closed" => GithubTimelineEventKind::Closed,
        "reopened" => GithubTimelineEventKind::Reopened,
        "head_ref_force_pushed" => GithubTimelineEventKind::HeadRefForcePushed,
        "cross-referenced" => GithubTimelineEventKind::CrossReferenced,
        "renamed" => GithubTimelineEventKind::Renamed,
        _ => return None,
    })
}

pub fn normalize_events(
    raw: &Value,
    cap: usize,
) -> Result<(Vec<GithubTimelineEvent>, bool), String> {
    let rows = raw
        .as_array()
        .ok_or_else(|| "timeline is not an array".to_owned())?;
    let mut events = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(event_name) = value_string(row, "event") else {
            continue;
        };
        let Some(kind) = timeline_kind(&event_name) else {
            continue;
        };
        let raw_at = if kind == GithubTimelineEventKind::Committed {
            nested(row, &["author", "date"]).and_then(Value::as_str)
        } else {
            row.get("created_at").and_then(Value::as_str)
        };
        let Some(created_at) = raw_at.and_then(parse_iso) else {
            continue;
        };
        let actor = if kind == GithubTimelineEventKind::Committed {
            nested(row, &["author", "name"])
                .and_then(Value::as_str)
                .unwrap_or("?")
        } else {
            nested(row, &["actor", "login"])
                .and_then(Value::as_str)
                .unwrap_or("?")
        };
        let identity = row
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string())
            .or_else(|| value_string(row, "sha"))
            .or_else(|| value_string(row, "node_id"))
            .unwrap_or_else(|| index.to_string());
        let mut mapped = GithubTimelineEvent {
            id: format!("evt-{identity}"),
            kind,
            actor: actor.to_owned(),
            avatar_url: if kind == GithubTimelineEventKind::Committed {
                None
            } else {
                nested(row, &["actor", "avatar_url"])
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            },
            created_at,
            url: None,
            sha: None,
            message: None,
            checks: None,
            label: None,
            subject: None,
            ref_number: None,
            ref_title: None,
            ref_is_pr: None,
        };
        match kind {
            GithubTimelineEventKind::Committed => {
                if let Some(sha) = value_string(row, "sha")
                    && sha.len() == 40
                    && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    mapped.sha = Some(sha);
                }
                if let Some(message) = value_string(row, "message") {
                    mapped.message = Some(
                        message
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(120)
                            .collect(),
                    );
                }
                mapped.url = value_string(row, "html_url");
            }
            GithubTimelineEventKind::Labeled | GithubTimelineEventKind::Unlabeled => {
                if let Some(label) = row.get("label")
                    && let Some(name) = value_string(label, "name")
                {
                    mapped.label = Some(GithubTimelineLabel {
                        name,
                        color: value_string(label, "color"),
                    });
                }
            }
            GithubTimelineEventKind::Assigned | GithubTimelineEventKind::Unassigned => {
                mapped.subject = nested(row, &["assignee", "login"])
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            GithubTimelineEventKind::Renamed => {
                mapped.subject = nested(row, &["rename", "to"])
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            GithubTimelineEventKind::CrossReferenced => {
                if let Some(issue) = nested(row, &["source", "issue"]) {
                    mapped.ref_number = issue.get("number").and_then(Value::as_u64);
                    mapped.ref_title = value_string(issue, "title");
                    mapped.ref_is_pr =
                        Some(issue.get("pull_request").is_some_and(|v| !v.is_null()));
                    mapped.url = value_string(issue, "html_url");
                }
            }
            _ => {}
        }
        events.push(mapped);
    }
    let truncated = events.len() > cap;
    if truncated {
        let start = events.len() - cap;
        events = events.split_off(start);
    }
    Ok((events, truncated))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelinePages {
    pub rows: Vec<Value>,
    pub stopped_short: bool,
}

pub fn fetch_timeline_pages<F, N>(
    mut run: F,
    max_pages: usize,
    budget_ms: u64,
    min_page_ms: u64,
    now: N,
) -> Result<TimelinePages, String>
where
    F: FnMut(usize, u64) -> Result<String, String>,
    N: Fn() -> u64,
{
    let deadline = now().saturating_add(budget_ms);
    let mut rows = Vec::new();
    let mut stopped_short = false;
    let mut page = 1;
    while page <= max_pages {
        let remaining = deadline.saturating_sub(now());
        if remaining < min_page_ms {
            stopped_short = true;
            break;
        }
        let raw = match run(page, remaining) {
            Ok(raw) => raw,
            Err(error) if page == 1 => return Err(error),
            Err(_) => {
                stopped_short = true;
                break;
            }
        };
        let parsed: Vec<Value> = match serde_json::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(error) if page == 1 => return Err(error.to_string()),
            Err(_) => {
                stopped_short = true;
                break;
            }
        };
        let short = parsed.len() < 100;
        rows.extend(parsed);
        if short {
            break;
        }
        page += 1;
    }
    if page > max_pages {
        stopped_short = true;
    }
    Ok(TimelinePages {
        rows,
        stopped_short,
    })
}

pub fn derive_pr_reference_status(
    state: &str,
    is_draft: bool,
    review_decision: Option<&str>,
    checks: Option<ChecksGlyph>,
    head_committed_at: Option<&str>,
    changes_requested_at: Option<&str>,
    review_requested: bool,
) -> ReferenceStatus {
    let state = state.to_ascii_uppercase();
    if state == "MERGED" {
        return ReferenceStatus::Merged;
    }
    if state == "CLOSED" {
        return ReferenceStatus::Closed;
    }
    if is_draft {
        return ReferenceStatus::Draft;
    }
    if checks == Some(ChecksGlyph::Pending) {
        return ReferenceStatus::ChecksPending;
    }
    let decision = review_decision.unwrap_or_default().to_ascii_uppercase();
    let changes_requested = decision == "CHANGES_REQUESTED";
    let answered = review_requested || pushed_since(head_committed_at, changes_requested_at);
    if changes_requested && !answered {
        return ReferenceStatus::ChangesRequested;
    }
    if checks == Some(ChecksGlyph::Failing) {
        return ReferenceStatus::ChecksFailing;
    }
    if decision == "APPROVED" {
        return ReferenceStatus::Ready;
    }
    if changes_requested || decision == "REVIEW_REQUIRED" || review_requested {
        return ReferenceStatus::ReviewRequired;
    }
    ReferenceStatus::Ready
}

fn pushed_since(head: Option<&str>, reviewed: Option<&str>) -> bool {
    let Some(head) = head.and_then(parse_datetime) else {
        return false;
    };
    let Some(reviewed) = reviewed.and_then(parse_datetime) else {
        return false;
    };
    head > reviewed
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

pub fn derive_issue_reference_status(state: &str, reason: Option<&str>) -> ReferenceStatus {
    if !state.eq_ignore_ascii_case("CLOSED") {
        return ReferenceStatus::Open;
    }
    if reason
        .unwrap_or_default()
        .eq_ignore_ascii_case("NOT_PLANNED")
    {
        ReferenceStatus::NotPlanned
    } else {
        ReferenceStatus::Completed
    }
}

pub fn sanitize_ref_numbers(values: &[u64], cap: usize) -> Vec<u64> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .copied()
        .filter(|value| *value > 0)
        .filter(|value| seen.insert(*value))
        .take(cap)
        .collect()
}

pub fn ref_number_from_url(url: &str) -> Option<u64> {
    let tail = url.trim().trim_end_matches('/').rsplit('/').next()?;
    let number = tail.parse::<u64>().ok()?;
    (number > 0).then_some(number)
}

pub fn ref_status_ttl(reference: Option<&ResolvedReference>) -> u64 {
    match reference.map(|entry| entry.status) {
        Some(ReferenceStatus::Merged) => REF_STATUS_MERGED_TTL,
        Some(
            ReferenceStatus::Closed | ReferenceStatus::Completed | ReferenceStatus::NotPlanned,
        ) => REF_STATUS_CLOSED_TTL,
        _ => CACHE_MS,
    }
}

pub fn ref_status_recheck_after(reference: Option<&ResolvedReference>) -> Option<u64> {
    if reference.is_some_and(|entry| entry.status == ReferenceStatus::Merged) {
        None
    } else {
        Some(ref_status_ttl(reference))
    }
}

pub fn batch_recheck_after(entries: &[Option<ResolvedReference>]) -> Option<u64> {
    entries
        .iter()
        .filter_map(|entry| ref_status_recheck_after(entry.as_ref()))
        .min()
}

pub fn has_resolved_repository(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.pointer("/data/repository").cloned())
        .is_some_and(|repository| repository.is_object() && !repository.is_null())
}

pub fn first_line(value: &str) -> String {
    value
        .lines()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| "gh failed".to_owned(), |line| line.trim().to_owned())
}

pub fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.trim().lines().collect();
    lines
        .iter()
        .rev()
        .take(3)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" | ")
        .chars()
        .take(300)
        .collect()
}

pub fn map_check_state(value: Option<&str>) -> coducktor_contract::GithubCheckState {
    match value.unwrap_or_default().to_ascii_uppercase().as_str() {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => coducktor_contract::GithubCheckState::Passing,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" => {
            coducktor_contract::GithubCheckState::Failing
        }
        "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "WAITING" | "REQUESTED" => {
            coducktor_contract::GithubCheckState::Pending
        }
        _ => coducktor_contract::GithubCheckState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::GithubItemKind;

    fn json(value: &str) -> Value {
        serde_json::from_str(value).expect("fixture JSON")
    }

    #[test]
    fn rollup_precedence_matches_github_driver() {
        assert_eq!(rollup_to_checks(None), None);
        assert_eq!(
            rollup_to_checks(Some(&[
                CheckRun {
                    state: None,
                    status: Some("IN_PROGRESS".into()),
                    conclusion: None
                },
                CheckRun {
                    state: None,
                    status: None,
                    conclusion: Some("FAILURE".into())
                },
            ])),
            Some(ChecksGlyph::Failing)
        );
        assert_eq!(
            rollup_to_checks(Some(&[CheckRun {
                state: None,
                status: None,
                conclusion: Some("SUCCESS".into())
            }])),
            Some(ChecksGlyph::Passing)
        );
    }

    #[test]
    fn comments_and_reviews_keep_the_wire_caps_and_filters() {
        let comments = normalize_comments(&json(
            r#"[{"id":7,"user":null,"created_at":"t","body":null,"html_url":"u"}]"#,
        ))
        .unwrap();
        assert_eq!(comments[0].author, "?");
        assert_eq!(comments[0].body, "");
        let reviews = normalize_reviews(&json(r#"[
          {"id":1,"user":{"login":"r"},"body":" ","state":"COMMENTED","submitted_at":"t","html_url":"u"},
          {"id":2,"user":{"login":"r"},"body":"","state":"APPROVED","submitted_at":"t","html_url":"u"}
        ]"#)).unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].kind, GithubCommentKind::Review);
    }

    #[test]
    fn counts_pages_accumulate_cursors_and_degrade_on_bad_envelopes() {
        let page = |root: &str, number: u64, cursor: Option<&str>| {
            let mut repository = serde_json::Map::new();
            repository.insert(root.to_owned(), serde_json::json!({"nodes":[{"number":number,"comments":{"totalCount":3}}],"pageInfo":{"hasNextPage":cursor.is_some(),"endCursor":cursor}}));
            serde_json::json!({"data":{"repository":repository}}).to_string()
        };
        let parsed = parse_counts_page(&page("issues", 7, Some("C1")), "issues").unwrap();
        assert_eq!(parsed.counts.get(&7), Some(&3));
        assert_eq!(parsed.end_cursor.as_deref(), Some("C1"));
        assert!(parse_counts_page("{\"data\":{}}", "issues").is_err());
        let mut calls = 0;
        let (issues, prs) = fetch_comment_counts(
            |query, vars| {
                calls += 1;
                if query.contains("pullRequests") {
                    Ok(page("pullRequests", 10, None))
                } else if vars.contains_key("endCursor") {
                    Ok(page("issues", 8, None))
                } else {
                    Ok(page("issues", 7, Some("C1")))
                }
            },
            "o",
            "n",
            3,
        );
        assert_eq!(calls, 3);
        assert_eq!(issues.len(), 2);
        assert_eq!(prs.get(&10), Some(&3));
    }

    #[test]
    fn timeline_commits_use_author_dates_and_keep_newest_window() {
        let rows: Vec<Value> = (0..3)
            .map(|index| json(&format!(r#"{{"event":"committed","sha":"{:0>40}","author":{{"name":"Ada","date":"2026-08-01T0{}:00:00+02:00"}},"message":"m\nmore"}}"#, index + 1, index)))
            .collect();
        let (events, truncated) = normalize_events(&Value::Array(rows), 2).unwrap();
        assert!(truncated);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].actor, "Ada");
        assert!(events[0].created_at.ends_with('Z'));
        assert_eq!(events[0].message.as_deref(), Some("m"));
        let _ = GithubItemKind::Issue;
    }

    #[test]
    fn reference_precedence_matches_the_task_chip() {
        assert_eq!(
            derive_pr_reference_status(
                "OPEN",
                false,
                Some("CHANGES_REQUESTED"),
                Some(ChecksGlyph::Pending),
                None,
                None,
                false
            ),
            ReferenceStatus::ChecksPending
        );
        assert_eq!(
            derive_pr_reference_status(
                "OPEN",
                false,
                Some("CHANGES_REQUESTED"),
                Some(ChecksGlyph::Failing),
                Some("2026-08-11T12:00:00Z"),
                Some("2026-08-11T09:00:00Z"),
                false
            ),
            ReferenceStatus::ChecksFailing
        );
        assert_eq!(
            derive_issue_reference_status("CLOSED", Some("NOT_PLANNED")),
            ReferenceStatus::NotPlanned
        );
    }

    #[test]
    fn timeline_page_budget_keeps_previous_pages_when_a_later_page_fails() {
        let now = 0;
        let result = fetch_timeline_pages(
            |page, _remaining| {
                if page == 1 {
                    Ok(
                        serde_json::to_string(&vec![serde_json::json!({"event":"labeled"}); 100])
                            .unwrap(),
                    )
                } else {
                    Err("offline".to_owned())
                }
            },
            10,
            100,
            2,
            || now,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 100);
        assert!(result.stopped_short);
    }

    #[test]
    fn pr_file_pages_stop_at_three_hundred_and_reject_bad_json() {
        let mut calls = 0;
        let rows = fetch_pr_file_pages(|page| {
            calls += 1;
            Ok(serde_json::to_string(&vec![serde_json::json!({"filename":page}); 100]).unwrap())
        })
        .unwrap();
        assert_eq!(rows.len(), GH_PR_DIFF_FILE_CAP);
        assert_eq!(calls, 3);
        assert!(fetch_pr_file_pages(|_| Ok("{\"files\":[]}".to_owned())).is_err());
    }

    #[test]
    fn reference_url_and_ttl_helpers_keep_terminal_statuses_distinct() {
        assert_eq!(
            ref_number_from_url("  https://github.com/o/r/pull/774/"),
            Some(774)
        );
        assert_eq!(ref_number_from_url("https://github.com/o/r/pull/0"), None);
        let merged = ResolvedReference {
            kind: crate::model::ReferenceKind::Pr,
            status: ReferenceStatus::Merged,
        };
        assert_eq!(ref_status_ttl(Some(&merged)), REF_STATUS_MERGED_TTL);
        assert_eq!(ref_status_recheck_after(Some(&merged)), None);
    }
}
