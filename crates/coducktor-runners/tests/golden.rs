use std::fs;
use std::path::{Path, PathBuf};

use coducktor_protocol::UiEvent;
use coducktor_runners::claude::{
    ClaudeUiMapperState, claude_turn_started, create_claude_ui_state, map_claude_message,
};
use coducktor_runners::codex::{CodexUiMapperState, create_codex_ui_state, map_codex_notification};
use coducktor_runners::opencode::{
    OpencodeUiMapperState, create_opencode_ui_state, map_opencode_event, opencode_session_started,
    opencode_turn_started,
};
use coducktor_runners::pi::{
    PiUiMapperState, create_pi_ui_state, map_pi_rpc_message, pi_turn_started,
};
use serde_json::{Value, json};

const CLAUDE_FIXTURES: &[&str] = &[
    "text-turn",
    "bash-and-screenshot",
    "thinking-edit-write-todo",
    "subagent-task",
    "failed-and-denied",
    "task-tools-plan",
];

const CODEX_FIXTURES: &[&str] = &[
    "text-turn",
    "reasoning-stream",
    "reasoning-snapshot-arrays",
    "command-lifecycle",
    "file-change-and-mcp",
    "todo-list",
    "turn-plan-updated",
    "turn-failed",
    "review-mode",
    "collab-agent-tool-call",
    "collab-tool-call",
    "sub-agent-activity",
];

const OPENCODE_FIXTURES: &[&str] = &[
    "text-turn",
    "tool-lifecycle",
    "todowrite-plan",
    "patch-and-step-finish",
    "subtask-nested",
    "subtask-overlapping",
    "session-error",
];

const OPENCODE_SESSION_ID: &str = "ses_01J8ZE00MAIN";

const PI_FIXTURES: &[&str] = &["rpc-lifecycle"];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn capability_matrix_references_every_normalized_runner() {
    let matrix = fs::read_to_string(fixture_root().join("CAPABILITY_MATRIX.md"))
        .expect("capability matrix must be checked in");
    for runner in ["Codex", "Claude", "OpenCode", "pi"] {
        assert!(matrix.contains(runner), "matrix must cover {runner}");
    }
    for fixture in [
        "text-turn",
        "command-lifecycle",
        "bash-and-screenshot",
        "tool-lifecycle",
        "rpc-lifecycle",
    ] {
        assert!(
            matrix.contains(fixture),
            "matrix must link fixture {fixture}"
        );
    }
}

/// Protocol mappers must make forward-compatible degradation a bounded local operation: a
/// provider adding an unrecognized frame cannot manufacture a normalized event or leave a
/// replay harness waiting for a response from another runner.
#[test]
fn malformed_and_unknown_provider_frames_are_noops() {
    let malformed = Value::String("not an object".to_owned());
    let unknown = json!({"type": "future.provider.frame"});

    let codex = create_codex_ui_state();
    for value in [&malformed, &unknown] {
        let mapped = map_codex_notification(value, &codex);
        assert!(mapped.events.is_empty());
        assert_eq!(mapped.state, codex);
    }

    let claude = create_claude_ui_state(None);
    for value in [&malformed, &unknown] {
        let mapped = map_claude_message(value, &claude);
        assert!(mapped.events.is_empty());
        assert_eq!(mapped.state, claude);
    }

    let opencode = create_opencode_ui_state();
    for value in [&malformed, &unknown] {
        let mapped = map_opencode_event(value, &opencode);
        assert!(mapped.events.is_empty());
        assert_eq!(mapped.state, opencode);
    }

    let pi = create_pi_ui_state();
    for value in [&malformed, &unknown] {
        let mapped = map_pi_rpc_message(value, &pi);
        assert!(mapped.events.is_empty());
        assert_eq!(mapped.state, pi);
    }
}

fn replay_claude(name: &str) -> Vec<UiEvent> {
    let path = fixture_root().join("claude").join(format!("{name}.ndjson"));
    let raw = fs::read_to_string(path).expect("fixture must be readable");
    let mut state = create_claude_ui_state(None);
    let mut events = Vec::new();
    fold_claude(claude_turn_started(&state), &mut state, &mut events);
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        fold_claude(map_claude_message(&value, &state), &mut state, &mut events);
    }
    events
}

fn fold_claude(
    mapped: coducktor_runners::claude::ClaudeUiMapping,
    state: &mut ClaudeUiMapperState,
    events: &mut Vec<UiEvent>,
) {
    *state = mapped.state;
    events.extend(mapped.events);
}

fn replay_codex(name: &str) -> Vec<UiEvent> {
    let path = fixture_root().join("codex").join(format!("{name}.ndjson"));
    let raw = fs::read_to_string(path).expect("fixture must be readable");
    let mut state = create_codex_ui_state();
    let mut events = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        fold_codex(
            map_codex_notification(&value, &state),
            &mut state,
            &mut events,
        );
    }
    events
}

fn fold_codex(
    mapped: coducktor_runners::codex::CodexUiMapping,
    state: &mut CodexUiMapperState,
    events: &mut Vec<UiEvent>,
) {
    *state = mapped.state;
    events.extend(mapped.events);
}

fn replay_opencode(name: &str) -> Vec<UiEvent> {
    let path = fixture_root()
        .join("opencode")
        .join(format!("{name}.ndjson"));
    let raw = fs::read_to_string(path).expect("fixture must be readable");
    let mut state = create_opencode_ui_state();
    let mut events = Vec::new();
    fold_opencode(
        opencode_session_started(OPENCODE_SESSION_ID, &state),
        &mut state,
        &mut events,
    );
    fold_opencode(opencode_turn_started(&state), &mut state, &mut events);
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        fold_opencode(map_opencode_event(&value, &state), &mut state, &mut events);
    }
    events
}

fn fold_opencode(
    mapped: coducktor_runners::opencode::OpencodeUiMapping,
    state: &mut OpencodeUiMapperState,
    events: &mut Vec<UiEvent>,
) {
    *state = mapped.state;
    events.extend(mapped.events);
}

fn replay_pi(name: &str) -> Vec<UiEvent> {
    let path = fixture_root().join("pi").join(format!("{name}.ndjson"));
    let raw = fs::read_to_string(path).expect("fixture must be readable");
    let mut state = create_pi_ui_state();
    let mut events = Vec::new();
    fold_pi(pi_turn_started(&state), &mut state, &mut events);
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        fold_pi(map_pi_rpc_message(&value, &state), &mut state, &mut events);
    }
    events
}

fn fold_pi(
    mapped: coducktor_runners::pi::PiUiMapping,
    state: &mut PiUiMapperState,
    events: &mut Vec<UiEvent>,
) {
    *state = mapped.state;
    events.extend(mapped.events);
}

fn assert_json_equivalent(left: &Value, right: &Value, context: &str) {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            assert_eq!(left.as_f64(), right.as_f64(), "numeric drift in {context}");
        }
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(left.len(), right.len(), "array drift in {context}");
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                assert_json_equivalent(left, right, &format!("{context}[{index}]"));
            }
        }
        (Value::Object(left), Value::Object(right)) => {
            assert_eq!(left.len(), right.len(), "object drift in {context}");
            for (key, right_value) in right {
                let left_value = left
                    .get(key)
                    .unwrap_or_else(|| panic!("missing {key:?} in {context}"));
                assert_json_equivalent(left_value, right_value, &format!("{context}.{key}"));
            }
        }
        _ => assert_eq!(left, right, "value drift in {context}"),
    }
}

#[test]
fn claude_fixtures_match_the_normalized_mapper_output() {
    for fixture in CLAUDE_FIXTURES {
        let actual = serde_json::to_value(replay_claude(fixture)).expect("events must serialize");
        let expected_path = fixture_root()
            .join("claude")
            .join(format!("{fixture}.expected.json"));
        let expected: Value = serde_json::from_slice(
            &fs::read(expected_path).expect("expected fixture must be readable"),
        )
        .expect("expected fixture must be JSON");
        assert_json_equivalent(&actual, &expected, fixture);
    }
}

#[test]
fn codex_fixtures_match_the_normalized_mapper_output() {
    for fixture in CODEX_FIXTURES {
        let actual = serde_json::to_value(replay_codex(fixture)).expect("events must serialize");
        let expected_path = fixture_root()
            .join("codex")
            .join(format!("{fixture}.expected.json"));
        let expected: Value = serde_json::from_slice(
            &fs::read(expected_path).expect("expected fixture must be readable"),
        )
        .expect("expected fixture must be JSON");
        assert_json_equivalent(&actual, &expected, fixture);
    }
}

#[test]
fn opencode_fixtures_match_the_normalized_mapper_output() {
    for fixture in OPENCODE_FIXTURES {
        let actual = serde_json::to_value(replay_opencode(fixture)).expect("events must serialize");
        let expected_path = fixture_root()
            .join("opencode")
            .join(format!("{fixture}.expected.json"));
        let expected: Value = serde_json::from_slice(
            &fs::read(expected_path).expect("expected fixture must be readable"),
        )
        .expect("expected fixture must be JSON");
        assert_json_equivalent(&actual, &expected, fixture);
    }
}

#[test]
fn pi_fixtures_match_the_normalized_mapper_output() {
    for fixture in PI_FIXTURES {
        let actual = serde_json::to_value(replay_pi(fixture)).expect("events must serialize");
        let expected_path = fixture_root()
            .join("pi")
            .join(format!("{fixture}.expected.json"));
        let expected: Value = serde_json::from_slice(
            &fs::read(expected_path).expect("expected fixture must be readable"),
        )
        .expect("expected fixture must be JSON");
        assert_json_equivalent(&actual, &expected, fixture);
    }
}
