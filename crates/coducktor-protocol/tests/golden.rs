use std::fs;
use std::path::{Path, PathBuf};

use coducktor_protocol::UiEvent;
use serde_json::Value;

fn expected_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("fixture directory must be readable") {
        let path = entry.expect("fixture entry must be readable").path();
        if path.is_dir() {
            expected_files(&path, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".expected.json"))
        {
            output.push(path);
        }
    }
}

#[test]
fn all_committed_backend_goldens_deserialize_without_loss() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut files = Vec::new();
    expected_files(&fixture_root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "golden fixture set must not be empty");

    for path in files {
        let bytes = fs::read(&path).expect("golden fixture must be readable");
        let original: Value = serde_json::from_slice(&bytes).expect("golden must be JSON");
        let events: Vec<UiEvent> = serde_json::from_value(original.clone())
            .unwrap_or_else(|error| panic!("{} did not deserialize: {error}", path.display()));
        let round_trip = serde_json::to_value(events).expect("UI events must serialize");
        assert_json_equivalent(&round_trip, &original, &path.display().to_string());
    }
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
