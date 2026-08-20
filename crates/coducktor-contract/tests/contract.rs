use std::fs;
use std::path::Path;

use coducktor_contract::compat::{catch_optional, catch_or_default, salvage_entries};
use coducktor_contract::{
    AgentConfigListing, AgentProfilesResponse, ConfigResponse, GithubChecksData, GithubData,
    GithubRefStatusData, HealthResponse, IdeDirectoryResponse, OpenTargetsResponse,
    ProjectsResponse, RunEvent, RunRecord, Runner, RunnerModelCatalogResponse, RunsIndexResponse,
    Skill, UiState, WorkflowsResponse, WorkspaceConfigResponse, WorkspaceUiState,
    WorkspaceUsageResponse, WorktreesResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let bytes = fs::read(path).expect("fixture must be readable");
    serde_json::from_slice(&bytes).expect("fixture must be valid JSON")
}

fn assert_round_trip<T>(name: &str)
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let original = fixture(name);
    let parsed: T = serde_json::from_value(original.clone()).expect("fixture must deserialize");
    let round_tripped = serde_json::to_value(parsed).expect("contract must serialize");
    assert_json_equivalent(&round_tripped, &original, name);
}

fn assert_json_equivalent(left: &Value, right: &Value, context: &str) {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            assert_eq!(
                left.as_f64(),
                right.as_f64(),
                "numeric value changed in {context}"
            );
        }
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(left.len(), right.len(), "array length changed in {context}");
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                assert_json_equivalent(left, right, &format!("{context}[{index}]"));
            }
        }
        (Value::Object(left), Value::Object(right)) => {
            assert_eq!(left.len(), right.len(), "object keys changed in {context}");
            for (key, right_value) in right {
                let left_value = left
                    .get(key)
                    .unwrap_or_else(|| panic!("missing key {key:?} in {context}"));
                assert_json_equivalent(left_value, right_value, &format!("{context}.{key}"));
            }
        }
        _ => assert_eq!(left, right, "value changed in {context}"),
    }
}

#[test]
fn captured_live_responses_round_trip_without_wire_drift() {
    assert_round_trip::<HealthResponse>("health.json");
    assert_round_trip::<Vec<coducktor_contract::ApiRun>>("runs.json");
    assert_round_trip::<WorkflowsResponse>("workflows.json");
    assert_round_trip::<ProjectsResponse>("projects.json");
    assert_round_trip::<WorkspaceConfigResponse>("workspace-config.json");
    assert_round_trip::<ConfigResponse>("config.json");
    assert_round_trip::<coducktor_contract::ProviderStatusResponse>("provider-status.json");
    assert_round_trip::<OpenTargetsResponse>("open-targets.json");
    assert_round_trip::<WorktreesResponse>("worktrees.json");
    assert_round_trip::<UiState>("workspace-ui-state.json");
    assert_round_trip::<AgentProfilesResponse>("agent-profiles.json");
    assert_round_trip::<AgentConfigListing>("agent-config.json");
    assert_round_trip::<WorkspaceUsageResponse>("workspace-usage.json");
    assert_round_trip::<RunnerModelCatalogResponse>("models.json");
    assert_round_trip::<GithubChecksData>("github-checks.json");
    assert_round_trip::<GithubRefStatusData>("github-ref-status.json");
    assert_round_trip::<GithubData>("github.json");
    assert_round_trip::<IdeDirectoryResponse>("ide-directory.json");
    assert_round_trip::<Vec<Skill>>("skills.json");
    assert_round_trip::<RunEvent>("run-event.json");
}

#[test]
fn loose_objects_preserve_unknown_fields() {
    let original = json!({
        "appearance": {"accent": "lime"},
        "futurePreference": {"enabled": true},
        "notifications": {"enabled": true, "future": [1, 2, 3]},
    });
    let parsed: WorkspaceUiState = serde_json::from_value(original.clone()).unwrap();

    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
}

#[test]
fn catch_helpers_degrade_bad_fields_without_dropping_records() {
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Record {
        #[serde(default, deserialize_with = "catch_or_default")]
        count: u64,
        #[serde(default, deserialize_with = "catch_optional")]
        label: Option<String>,
    }

    let record: Record = serde_json::from_value(json!({
        "count": "not a number",
        "label": 42,
    }))
    .unwrap();

    assert_eq!(record, Record::default());
}

#[test]
fn salvage_helper_drops_only_invalid_entries() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Entry {
        id: String,
    }

    #[derive(Deserialize)]
    struct Entries {
        #[serde(deserialize_with = "salvage_entries")]
        entries: Vec<Entry>,
    }

    let parsed: Entries = serde_json::from_value(json!({
        "entries": [{"id": "keep"}, {"id": 42}, {"other": true}],
    }))
    .unwrap();

    assert_eq!(
        parsed.entries,
        vec![Entry {
            id: "keep".to_owned()
        }]
    );
}

#[test]
fn runs_index_reference_map_is_json_object_key_compatible() {
    let value = json!({
        "runs": [],
        "referenceStatuses": {"shop": {"prs": {"42": "ready"}, "issues": {}}},
        "perProjectLimit": 50,
        "truncated": [],
    });
    let parsed: RunsIndexResponse = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn legacy_run_provenance_remains_readable() {
    let original = json!({
        "id": "run-1",
        "title": "legacy task",
        "workflow": "quick-task",
        "task": "inspect the repository",
        "status": "done",
        "createdAt": "2026-08-15T21:27:27.000Z",
        "tokensUsed": 12,
        "archived": false,
        "steps": [{
            "id": "task",
            "name": "Do the task",
            "kind": "agent",
            "status": "done",
            "iterations": 1,
            "tokensUsed": 12,
        }],
        "automation": {
            "automationId": "nightly",
            "automationRevision": 3,
            "receiptId": "receipt-1",
            "event": "issues.opened",
            "githubUrl": "https://github.com/mock/repo/issues/1",
        },
    });
    let mut parsed: RunRecord = serde_json::from_value(original.clone()).unwrap();

    assert_json_equivalent(
        &serde_json::to_value(&parsed).unwrap(),
        &original,
        "legacy run",
    );
    assert_eq!(parsed.automation.take().unwrap().event, "issues.opened");
}

#[test]
fn routing_contracts_accept_opencode_and_preserve_sanitized_explanations() {
    let value = json!({
        "selected": {
            "runner": "opencode",
            "profileId": "default",
            "upstreamProvider": "anthropic",
            "model": "claude-sonnet",
            "reasoningEffort": "high",
            "routeKey": "opencode:default:anthropic/claude-sonnet"
        },
        "considered": [{
            "routeKey": "claude:default:sonnet",
            "runner": "claude",
            "profileId": "default",
            "model": "sonnet",
            "eligible": false,
            "reason": "reserved_quota"
        }],
        "generation": 1
    });
    let parsed: coducktor_contract::RoutingDecision =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(parsed.selected.as_ref().unwrap().runner, Runner::OpenCode);
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn old_unknown_usage_policy_spellings_remain_readable() {
    let allow: coducktor_contract::UnknownUsagePolicy =
        serde_json::from_value(json!("allow")).unwrap();
    let deny: coducktor_contract::UnknownUsagePolicy =
        serde_json::from_value(json!("deny")).unwrap();
    assert_eq!(
        allow,
        coducktor_contract::UnknownUsagePolicy::AllowWithPenalty
    );
    assert_eq!(deny, coducktor_contract::UnknownUsagePolicy::Exclude);
    assert_eq!(
        serde_json::to_value(allow).unwrap(),
        json!("allow_with_penalty")
    );
    assert_eq!(serde_json::to_value(deny).unwrap(), json!("exclude"));
}
