//! AskUser payload — the structured multiple-choice question an agent asks the user, so a
//! client can render clickable option chips instead of the prose fallback.
//!
//! The agent emits this as a trailing `DUCK:ASK <compact-json>` control marker (a sibling of
//! `DUCK:DONE` / `DUCK:MONITORING`), detected on the *assembled* turn text so delta-streaming
//! backends can't split it. The legacy `DUCK:ASK` spelling parses identically (dual-read shim,
//! the compatibility regex). [`super::super::workflows::run::decide_turn_marker`]'s `valid_ask`
//! parameter is
//! exactly [`parse_ask_marker`] returning `Some`.
//!
//! A malformed `DUCK:ASK` payload degrades the whole marker to plain text (never a
//! partially-valid card), so [`validate_ask_request`] is a plain all-or-nothing structural walk
//! over the decoded JSON.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::task_markers::canonicalize_markers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskQuestion {
    /// Stable key for the answer; defaults to the array index when omitted.
    pub id: Option<String>,
    /// <=12-char chip label (matches Claude Code's built-in `AskUserQuestion`'s `header`).
    pub header: String,
    pub question: String,
    pub options: Vec<AskOption>,
    pub multi_select: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    pub questions: Vec<AskQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskPathSegment {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskParseIssue {
    pub code: String,
    pub path: Vec<AskPathSegment>,
    pub message: String,
}

/// The diagnostic parse result used at turn-end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskMarkerParseResult {
    None,
    InvalidJson {
        message: String,
    },
    InvalidStructure {
        issues: Vec<AskParseIssue>,
    },
    Valid {
        request: AskRequest,
        normalized: bool,
    },
}

fn issue(path: Vec<AskPathSegment>, code: &str, message: impl Into<String>) -> AskParseIssue {
    AskParseIssue {
        code: code.to_owned(),
        path,
        message: message.into(),
    }
}

fn push_at(
    issues: &mut Vec<AskParseIssue>,
    mut path: Vec<AskPathSegment>,
    key: &str,
    code: &str,
    message: impl Into<String>,
) {
    path.push(AskPathSegment::Key(key.to_owned()));
    issues.push(issue(path, code, message));
}

fn validate_option(
    value: &Value,
    path: Vec<AskPathSegment>,
) -> Result<AskOption, Vec<AskParseIssue>> {
    let mut issues = Vec::new();
    let Some(obj) = value.as_object() else {
        return Err(vec![issue(path, "invalid_type", "expected an object")]);
    };
    for key in obj.keys() {
        if key != "label" && key != "description" {
            push_at(
                &mut issues,
                path.clone(),
                key,
                "unrecognized_keys",
                "unrecognized key",
            );
        }
    }
    let label = match obj.get("label").and_then(Value::as_str) {
        Some(s) if (1..=60).contains(&s.chars().count()) => s.to_owned(),
        _ => {
            push_at(
                &mut issues,
                path.clone(),
                "label",
                "invalid_string",
                "label must be 1-60 chars",
            );
            String::new()
        }
    };
    let description = match obj.get("description") {
        None => None,
        Some(Value::String(s)) if s.chars().count() <= 280 => Some(s.clone()),
        Some(_) => {
            push_at(
                &mut issues,
                path.clone(),
                "description",
                "invalid_string",
                "description must be at most 280 chars",
            );
            None
        }
    };
    if issues.is_empty() {
        Ok(AskOption { label, description })
    } else {
        Err(issues)
    }
}

fn validate_question(
    value: &Value,
    path: Vec<AskPathSegment>,
) -> Result<AskQuestion, Vec<AskParseIssue>> {
    let mut issues = Vec::new();
    let Some(obj) = value.as_object() else {
        return Err(vec![issue(path, "invalid_type", "expected an object")]);
    };
    for key in obj.keys() {
        if !["id", "header", "question", "options", "multiSelect"].contains(&key.as_str()) {
            push_at(
                &mut issues,
                path.clone(),
                key,
                "unrecognized_keys",
                "unrecognized key",
            );
        }
    }
    let id = match obj.get("id") {
        None => None,
        Some(Value::String(s)) if (1..=64).contains(&s.chars().count()) => Some(s.clone()),
        Some(_) => {
            push_at(
                &mut issues,
                path.clone(),
                "id",
                "invalid_string",
                "id must be 1-64 chars",
            );
            None
        }
    };
    let header = match obj.get("header").and_then(Value::as_str) {
        Some(s) if (1..=12).contains(&s.chars().count()) => s.to_owned(),
        _ => {
            push_at(
                &mut issues,
                path.clone(),
                "header",
                "invalid_string",
                "header must be 1-12 chars",
            );
            String::new()
        }
    };
    let question = match obj.get("question").and_then(Value::as_str) {
        Some(s) if (1..=400).contains(&s.chars().count()) => s.to_owned(),
        _ => {
            push_at(
                &mut issues,
                path.clone(),
                "question",
                "invalid_string",
                "question must be 1-400 chars",
            );
            String::new()
        }
    };
    let multi_select = match obj.get("multiSelect") {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            push_at(
                &mut issues,
                path.clone(),
                "multiSelect",
                "invalid_type",
                "multiSelect must be a boolean",
            );
            None
        }
    };
    let mut options_path = path.clone();
    options_path.push(AskPathSegment::Key("options".to_owned()));
    let mut options = Vec::new();
    match obj.get("options").and_then(Value::as_array) {
        None => issues.push(issue(options_path, "invalid_type", "options is required")),
        Some(items) => {
            if items.len() < 2 {
                issues.push(issue(
                    options_path.clone(),
                    "too_small",
                    "at least 2 options are required",
                ));
            }
            if items.len() > 4 {
                issues.push(issue(
                    options_path.clone(),
                    "too_big",
                    "at most 4 options are allowed",
                ));
            }
            for (i, item) in items.iter().enumerate() {
                let mut opt_path = options_path.clone();
                opt_path.push(AskPathSegment::Index(i));
                match validate_option(item, opt_path) {
                    Ok(opt) => options.push(opt),
                    Err(mut opt_issues) => issues.append(&mut opt_issues),
                }
            }
            if issues.is_empty() {
                let mut seen = HashSet::new();
                for opt in &options {
                    if !seen.insert(opt.label.clone()) {
                        issues.push(issue(
                            options_path.clone(),
                            "custom",
                            "option labels must be unique within a question",
                        ));
                        break;
                    }
                }
            }
        }
    }
    if issues.is_empty() {
        Ok(AskQuestion {
            id,
            header,
            question,
            options,
            multi_select,
        })
    } else {
        Err(issues)
    }
}

/// Parse a value into a validated [`AskRequest`], or `Err` (with diagnostics) when it does not
/// match — bad counts, over-length fields, non-unique labels/questions, or extra keys.
pub fn validate_ask_request(value: &Value) -> Result<AskRequest, Vec<AskParseIssue>> {
    let mut issues = Vec::new();
    let Some(obj) = value.as_object() else {
        return Err(vec![issue(vec![], "invalid_type", "expected an object")]);
    };
    for key in obj.keys() {
        if key != "questions" {
            push_at(
                &mut issues,
                vec![],
                key,
                "unrecognized_keys",
                "unrecognized key",
            );
        }
    }
    let questions_path = vec![AskPathSegment::Key("questions".to_owned())];
    let Some(items) = obj.get("questions").and_then(Value::as_array) else {
        issues.push(issue(
            questions_path,
            "invalid_type",
            "questions is required",
        ));
        return Err(issues);
    };
    if items.is_empty() {
        issues.push(issue(
            questions_path.clone(),
            "too_small",
            "at least 1 question is required",
        ));
    }
    if items.len() > 4 {
        issues.push(issue(
            questions_path.clone(),
            "too_big",
            "at most 4 questions are allowed",
        ));
    }
    let mut questions = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let mut q_path = questions_path.clone();
        q_path.push(AskPathSegment::Index(i));
        match validate_question(item, q_path) {
            Ok(q) => questions.push(q),
            Err(mut q_issues) => issues.append(&mut q_issues),
        }
    }
    if issues.is_empty() {
        let mut seen = HashSet::new();
        for q in &questions {
            if !seen.insert(q.question.clone()) {
                issues.push(issue(
                    questions_path.clone(),
                    "custom",
                    "question texts must be unique",
                ));
                break;
            }
        }
    }
    if issues.is_empty() {
        Ok(AskRequest { questions })
    } else {
        Err(issues)
    }
}

/// `parseAskRequest` — validates a decoded value, discarding diagnostics.
pub fn parse_ask_request(value: &Value) -> Option<AskRequest> {
    validate_ask_request(value).ok()
}

/// Recover only presentation drift that cannot change the user's available choices. Unknown
/// keys are discarded, and the display-only header and option description are clipped to their
/// documented bounds. Counts, required text, types, and uniqueness remain enforced by
/// [`validate_ask_request`]; questions/options are never dropped to manufacture validity.
fn normalize_ask_request(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut request = Map::new();
    let Some(questions) = obj.get("questions") else {
        return Value::Object(request);
    };
    let normalized = match questions.as_array() {
        Some(items) => Value::Array(items.iter().map(normalize_question).collect()),
        None => questions.clone(),
    };
    request.insert("questions".to_owned(), normalized);
    Value::Object(request)
}

fn normalize_question(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut next = Map::new();
    for key in ["id", "question", "multiSelect"] {
        if let Some(v) = obj.get(key) {
            next.insert(key.to_owned(), v.clone());
        }
    }
    if let Some(header) = obj.get("header") {
        let normalized = match header {
            Value::String(s) => Value::String(s.chars().take(12).collect()),
            other => other.clone(),
        };
        next.insert("header".to_owned(), normalized);
    }
    if let Some(options) = obj.get("options") {
        let normalized = match options.as_array() {
            Some(items) => Value::Array(items.iter().map(normalize_option).collect()),
            None => options.clone(),
        };
        next.insert("options".to_owned(), normalized);
    }
    Value::Object(next)
}

fn normalize_option(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut next = Map::new();
    if let Some(label) = obj.get("label") {
        next.insert("label".to_owned(), label.clone());
    }
    if let Some(description) = obj.get("description") {
        let normalized = match description {
            Value::String(s) => Value::String(s.chars().take(280).collect()),
            other => other.clone(),
        };
        next.insert("description".to_owned(), normalized);
    }
    Value::Object(next)
}

/// The AskUser control marker: a trailing `DUCK:ASK <compact-json>` line. Detected on the
/// *assembled* turn text so delta-streaming backends can't split it — uniform across every
/// runner. The JSON is greedily captured from the first `{` after the keyword to the end of the
/// (already right-trimmed) text.
static ASK_MARKER_CANDIDATE_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"DUCK:ASK[ \t]+([\s\S]*)$"));

/// Stricter twin used only to strip a marker for display — the captured group must look like a
/// JSON object, and any whitespace/newline right before the marker goes with it.
static ASK_MARKER_STRIP_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\s*DUCK:ASK[ \t]+\{[\s\S]*\}\s*$"));

/// Parse a trailing marker with an actionable result for diagnostics. A parseable near-valid
/// request gets one bounded normalization pass; structural violations remain rejected so the
/// raw fallback stays readable.
pub fn parse_ask_marker_result(turn_text: &str) -> AskMarkerParseResult {
    let canonical = canonicalize_markers(turn_text);
    let trimmed = canonical.trim_end();
    let Some(captures) = ASK_MARKER_CANDIDATE_RE
        .as_ref()
        .ok()
        .and_then(|regex| regex.captures(trimmed))
    else {
        return AskMarkerParseResult::None;
    };
    let raw: Value = match serde_json::from_str(&captures[1]) {
        Ok(value) => value,
        Err(error) => {
            return AskMarkerParseResult::InvalidJson {
                message: error.to_string(),
            };
        }
    };
    if let Ok(request) = validate_ask_request(&raw) {
        return AskMarkerParseResult::Valid {
            request,
            normalized: false,
        };
    }
    let normalized_value = normalize_ask_request(&raw);
    match validate_ask_request(&normalized_value) {
        Ok(request) => AskMarkerParseResult::Valid {
            request,
            normalized: true,
        },
        Err(issues) => AskMarkerParseResult::InvalidStructure { issues },
    }
}

/// Extract and validate a trailing `DUCK:ASK <json>` marker (or its legacy `DUCK:ASK` twin) from
/// assembled turn text. `None` when there is no marker or its payload remains invalid — callers
/// degrade to plain text, the prose fallback is never made worse. This is also
/// [`super::super::workflows::run::decide_turn_marker`]'s `valid_ask` input: `is_some()`.
pub fn parse_ask_marker(turn_text: &str) -> Option<AskRequest> {
    match parse_ask_marker_result(turn_text) {
        AskMarkerParseResult::Valid { request, .. } => Some(request),
        _ => None,
    }
}

/// Strip a trailing `DUCK:ASK <json>` marker (or its legacy `DUCK:ASK` twin) from one text event
/// so transcripts stay free of protocol noise — but ONLY when the payload actually validates. An
/// invalid payload never becomes an ask card, so stripping it would delete the agent's question
/// from the transcript with nothing to replace it; it stays visible as raw text instead.
pub fn strip_ask_marker(text: &str) -> String {
    let canonical = canonicalize_markers(text);
    if parse_ask_marker(&canonical).is_none() {
        return text.to_owned();
    }
    ASK_MARKER_STRIP_RE
        .as_ref()
        .map_or(canonical.clone(), |regex| {
            regex.replace(&canonical, "").into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_request() -> AskRequest {
        AskRequest {
            questions: vec![AskQuestion {
                id: None,
                header: "Library".to_owned(),
                question: "Which date library should I standardize on?".to_owned(),
                options: vec![
                    AskOption {
                        label: "date-fns".to_owned(),
                        description: Some("Tree-shakeable".to_owned()),
                    },
                    AskOption {
                        label: "Luxon".to_owned(),
                        description: Some("Immutable, tz-aware".to_owned()),
                    },
                ],
                multi_select: None,
            }],
        }
    }

    fn valid_json() -> Value {
        json!({
            "questions": [{
                "header": "Library",
                "question": "Which date library should I standardize on?",
                "options": [
                    {"label": "date-fns", "description": "Tree-shakeable"},
                    {"label": "Luxon", "description": "Immutable, tz-aware"},
                ],
            }],
        })
    }

    #[test]
    fn accepts_the_legacy_marker_spelling() {
        let legacy = concat!("C", "E", "Z");
        let text = format!(
            "Pick one.\n{legacy}:ASK {}",
            serde_json::to_string(&valid_json()).unwrap()
        );
        assert_eq!(parse_ask_marker(&text), Some(valid_request()));
    }

    #[test]
    fn accepts_a_well_formed_single_question_request() {
        assert_eq!(parse_ask_request(&valid_json()), Some(valid_request()));
    }

    #[test]
    fn accepts_up_to_four_questions_with_multi_select_and_optional_descriptions() {
        let req = json!({
            "questions": [
                {
                    "id": "q1",
                    "header": "Sections",
                    "question": "Which sections?",
                    "multiSelect": true,
                    "options": [{"label": "Profile"}, {"label": "Billing"}],
                },
                {
                    "header": "Theme",
                    "question": "Which theme?",
                    "options": [{"label": "Light"}, {"label": "Dark"}, {"label": "System"}],
                },
            ],
        });
        assert!(parse_ask_request(&req).is_some());
    }

    #[test]
    fn rejects_an_empty_questions_array() {
        assert_eq!(parse_ask_request(&json!({"questions": []})), None);
    }

    #[test]
    fn rejects_more_than_four_questions() {
        let q =
            json!({"header": "H", "question": "Q?", "options": [{"label": "a"}, {"label": "b"}]});
        assert_eq!(
            parse_ask_request(&json!({"questions": [q, q, q, q, q]})),
            None
        );
    }

    #[test]
    fn rejects_a_question_with_fewer_than_two_options() {
        let req = json!({"questions": [{"header": "H", "question": "Q?", "options": [{"label": "only"}]}]});
        assert_eq!(parse_ask_request(&req), None);
    }

    #[test]
    fn rejects_a_header_longer_than_twelve_chars() {
        let req = json!({"questions": [{
            "header": "thirteen char",
            "question": "Q?",
            "options": [{"label": "a"}, {"label": "b"}],
        }]});
        assert_eq!(parse_ask_request(&req), None);
    }

    #[test]
    fn rejects_non_unique_option_labels_within_a_question() {
        let req = json!({"questions": [{
            "header": "H",
            "question": "Q?",
            "options": [{"label": "same"}, {"label": "same"}],
        }]});
        assert_eq!(parse_ask_request(&req), None);
    }

    #[test]
    fn rejects_non_unique_question_texts() {
        let q =
            json!({"header": "H", "question": "Q?", "options": [{"label": "a"}, {"label": "b"}]});
        assert_eq!(
            parse_ask_request(&json!({"questions": [q.clone(), q]})),
            None
        );
    }

    #[test]
    fn rejects_unknown_top_level_and_per_option_keys() {
        let questions = valid_json()["questions"].clone();
        assert_eq!(
            parse_ask_request(&json!({"questions": questions, "extra": 1})),
            None
        );
        let req = json!({"questions": [{
            "header": "H",
            "question": "Q?",
            "options": [{"label": "a", "color": "red"}, {"label": "b"}],
        }]});
        assert_eq!(parse_ask_request(&req), None);
    }

    #[test]
    fn rejects_non_object_input() {
        assert_eq!(parse_ask_request(&Value::Null), None);
        assert_eq!(parse_ask_request(&json!("DUCK:ASK")), None);
        assert_eq!(parse_ask_request(&json!(42)), None);
    }

    fn ask_json() -> String {
        serde_json::to_string(&valid_json()).unwrap()
    }

    #[test]
    fn extracts_a_valid_request_from_a_trailing_legacy_ask_marker() {
        let turn = format!("Here are the options.\nDUCK:ASK {}", ask_json());
        assert_eq!(parse_ask_marker(&turn), Some(valid_request()));
    }

    #[test]
    fn accepts_the_duck_ask_spelling() {
        let turn = format!("Here are the options.\nDUCK:ASK {}", ask_json());
        assert_eq!(parse_ask_marker(&turn), Some(valid_request()));
    }

    #[test]
    fn tolerates_trailing_whitespace_after_the_json() {
        let turn = format!("text\nDUCK:ASK {}\n  \n", ask_json());
        assert_eq!(parse_ask_marker(&turn), Some(valid_request()));
    }

    #[test]
    fn returns_none_when_there_is_no_marker() {
        assert_eq!(parse_ask_marker("just a normal answer, no marker"), None);
    }

    #[test]
    fn returns_none_on_malformed_json() {
        assert_eq!(parse_ask_marker("DUCK:ASK {not json"), None);
    }

    #[test]
    fn returns_none_when_json_is_valid_but_fails_the_schema() {
        assert_eq!(parse_ask_marker("DUCK:ASK {\"questions\":[]}"), None);
    }

    #[test]
    fn ignores_a_marker_that_is_not_at_the_end_of_the_turn() {
        let turn = format!("DUCK:ASK {}\nand then more text after", ask_json());
        assert_eq!(parse_ask_marker(&turn), None);
    }

    #[test]
    fn normalizes_bounded_presentation_drift_without_changing_the_choices() {
        let description = "d".repeat(300);
        let payload = json!({
            "transportHint": "render as chips",
            "questions": [{
                "header": "Implementation path",
                "question": "Which implementation should I use?",
                "multiSelect": false,
                "presentation": "compact",
                "options": [
                    {"label": "Minimal", "description": description, "recommended": true},
                    {"label": "Expanded", "description": "Touch the wider surface"},
                ],
            }],
        });
        let turn = format!("DUCK:ASK {}", serde_json::to_string(&payload).unwrap());
        let AskMarkerParseResult::Valid {
            request,
            normalized,
        } = parse_ask_marker_result(&turn)
        else {
            panic!("expected a normalized valid result");
        };
        assert!(normalized);
        assert_eq!(request.questions[0].header, "Implementati");
        assert_eq!(
            request.questions[0].question,
            "Which implementation should I use?"
        );
        assert_eq!(request.questions[0].options[0].label, "Minimal");
        assert_eq!(
            request.questions[0].options[0].description,
            Some("d".repeat(280))
        );
        assert_eq!(request.questions[0].options[1].label, "Expanded");
    }

    #[test]
    fn reports_the_questions_path_for_a_hard_structural_failure() {
        let result = parse_ask_marker_result("DUCK:ASK {\"questions\":[]}");
        let AskMarkerParseResult::InvalidStructure { issues } = result else {
            panic!("expected an invalid-structure result");
        };
        assert_eq!(
            issues[0].path,
            vec![AskPathSegment::Key("questions".to_owned())]
        );
    }

    #[test]
    fn never_drops_options_to_force_an_over_cap_request_to_validate() {
        let options: Vec<Value> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|label| json!({"label": label}))
            .collect();
        let payload = json!({"questions": [{
            "header": "Choice",
            "question": "Which?",
            "options": options,
        }]});
        let turn = format!("DUCK:ASK {}", serde_json::to_string(&payload).unwrap());
        assert!(matches!(
            parse_ask_marker_result(&turn),
            AskMarkerParseResult::InvalidStructure { .. }
        ));
    }

    #[test]
    fn strip_ask_marker_removes_a_trailing_legacy_ask_marker() {
        let turn = format!("Pick one.\nDUCK:ASK {}", ask_json());
        assert_eq!(strip_ask_marker(&turn), "Pick one.");
    }

    #[test]
    fn strip_ask_marker_removes_a_trailing_duck_ask_marker() {
        let turn = format!("Pick one.\nDUCK:ASK {}", ask_json());
        assert_eq!(strip_ask_marker(&turn), "Pick one.");
    }

    #[test]
    fn strip_ask_marker_leaves_text_without_a_marker_untouched() {
        assert_eq!(strip_ask_marker("no marker here"), "no marker here");
    }

    #[test]
    fn strip_ask_marker_keeps_a_marker_that_fails_the_schema() {
        let invalid = "Pick one.\nDUCK:ASK {\"questions\":[]}";
        assert_eq!(strip_ask_marker(invalid), invalid);
    }

    #[test]
    fn strip_ask_marker_strips_after_clipping_an_over_length_header() {
        let payload = json!({"questions": [{
            "header": "thirteen char",
            "question": "Which variant?",
            "options": [{"label": "A"}, {"label": "B"}],
        }]});
        let text = format!(
            "Before we continue:\nDUCK:ASK {}",
            serde_json::to_string(&payload).unwrap()
        );
        assert_eq!(strip_ask_marker(&text), "Before we continue:");
    }

    #[test]
    fn strip_ask_marker_keeps_a_marker_whose_payload_is_not_valid_json() {
        let invalid = "Pick one.\nDUCK:ASK {\"questions\": [}";
        assert_eq!(strip_ask_marker(invalid), invalid);
    }

    #[test]
    fn detects_a_marker_assembled_from_many_delta_chunks() {
        let full = format!("Let me confirm the approach.\n\nDUCK:ASK {}", ask_json());
        let chunks: Vec<&str> = {
            let mut out = Vec::new();
            let bytes = full.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let end = (i + 7).min(bytes.len());
                out.push(&full[i..end]);
                i = end;
            }
            out
        };
        assert!(chunks.len() > 1);
        let assembled: String = chunks.concat();
        assert_eq!(parse_ask_marker(&assembled), Some(valid_request()));
    }
}
