use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const BACKENDS: &[&str] = &["claude", "codex", "opencode", "pi"];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/cezar/src/core/__fixtures__")
}

fn fixture_events(backend: &str) -> Vec<Value> {
    let directory = fixture_root().join(backend);
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .expect("backend fixture directory must be readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".expected.json"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .flat_map(|path| {
            let bytes = fs::read(path).expect("expected fixture must be readable");
            serde_json::from_slice::<Vec<Value>>(&bytes).expect("expected fixture must be JSON")
        })
        .collect()
}

fn items(events: &[Value]) -> impl Iterator<Item = &Value> {
    events.iter().filter_map(|event| {
        matches!(
            event.get("type").and_then(Value::as_str),
            Some("item.started" | "item.updated" | "item.completed")
        )
        .then(|| event.get("item"))
        .flatten()
    })
}

fn has_tool_status(events: &[Value], status: &str) -> bool {
    items(events).any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item.get("status").and_then(Value::as_str) == Some(status)
    })
}

fn has_nonempty_plan(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("plan.updated")
            && event
                .get("entries")
                .and_then(Value::as_array)
                .is_some_and(|entries| !entries.is_empty())
    })
}

fn has_nonempty_reasoning(events: &[Value]) -> bool {
    items(events).any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("reasoning")
            && item
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
    })
}

fn has_structured_diff(events: &[Value]) -> bool {
    items(events).any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item
                .get("diffs")
                .and_then(Value::as_array)
                .is_some_and(|diffs| !diffs.is_empty())
    })
}

fn has_task_item(events: &[Value]) -> bool {
    items(events).any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("tool")
            && item.get("toolKind").and_then(Value::as_str) == Some("task")
    })
}

fn has_usage_update(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("usage.updated")
            && event
                .get("usage")
                .and_then(|usage| usage.get("total"))
                .and_then(Value::as_f64)
                .is_some_and(|total| total > 0.0)
    })
}

fn has_per_turn_usage(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("turn.completed")
            && event
                .get("usage")
                .and_then(|usage| usage.get("input"))
                .and_then(Value::as_f64)
                .is_some_and(|input| input > 0.0)
            && event
                .get("usage")
                .and_then(|usage| usage.get("output"))
                .and_then(Value::as_f64)
                .is_some_and(|output| output > 0.0)
    })
}

fn has_stop_reason(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("turn.completed")
            && event.get("stopReason").is_some()
    })
}

type Capability = (&'static str, fn(&[Value]) -> bool);

#[test]
fn every_backend_produces_each_parity_capability() {
    let capabilities: &[Capability] = &[
        ("plan.updated with entries", has_nonempty_plan),
        ("tool status running", |events| {
            has_tool_status(events, "running")
        }),
        ("tool status completed", |events| {
            has_tool_status(events, "completed")
        }),
        ("tool status failed", |events| {
            has_tool_status(events, "failed")
        }),
        ("non-empty reasoning", has_nonempty_reasoning),
        ("structured diffs", has_structured_diff),
        ("sub-agent task item", has_task_item),
        ("usage.updated with raw counts", has_usage_update),
        ("turn.completed with directional usage", has_per_turn_usage),
        ("turn.completed with stop reason", has_stop_reason),
    ];
    for backend in BACKENDS {
        let events = fixture_events(backend);
        for (name, predicate) in capabilities {
            assert!(predicate(&events), "{backend} is missing {name}");
        }
    }
}

#[test]
fn wire_attributed_subagents_remain_nested() {
    for backend in ["claude", "opencode"] {
        let events = fixture_events(backend);
        assert!(
            items(&events).any(|item| item.get("parentItemId").is_some()),
            "{backend} has no parentItemId nesting"
        );
    }
}
