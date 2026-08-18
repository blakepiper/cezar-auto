//! Pure semantic presenters for normalized tool items.

use std::path::{Path, PathBuf};

use coducktor_protocol::{ToolKind, ToolStatus, UiToolItem};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentation {
    pub title: String,
    pub subject: Option<String>,
    pub status: ToolStatus,
    pub preview: Option<String>,
    pub detail: Option<String>,
    pub changed_files: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
}

pub fn present_tool(tool: &UiToolItem, project_root: Option<&Path>) -> ToolPresentation {
    let subject = subject(tool, project_root);
    let title = match tool.tool_kind {
        ToolKind::Read => format!("Read {}", subject.as_deref().unwrap_or("files")),
        ToolKind::Search => format!("Searched {}", quoted_subject(tool, subject.as_deref())),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            format!("Edited {}", subject.as_deref().unwrap_or("files"))
        }
        ToolKind::Execute => format!("{} {}", status_verb(tool.status), execute_subject(tool)),
        ToolKind::Fetch => format!("Fetched {}", subject.as_deref().unwrap_or("resource")),
        ToolKind::Task => format!(
            "Task{}",
            subject
                .as_ref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        ),
        ToolKind::Think => "Think".to_owned(),
        ToolKind::Plan => "Plan updated".to_owned(),
        ToolKind::Other => tool.name.to_string(),
    };
    let (changed_files, added_lines, removed_lines) =
        tool.diffs.as_deref().map(diff_counts).unwrap_or_default();
    ToolPresentation {
        title,
        subject,
        status: tool.status,
        preview: tool
            .error
            .as_deref()
            .or(tool.output.as_deref())
            .map(|value| sanitize_bounded(value, 240)),
        detail: detail(tool),
        changed_files,
        added_lines,
        removed_lines,
    }
}

fn subject(tool: &UiToolItem, project_root: Option<&Path>) -> Option<String> {
    let path = tool
        .locations
        .as_deref()
        .and_then(|locations| locations.first())
        .map(|location| location.path.clone())
        .or_else(|| {
            tool.input.as_ref().and_then(|input| {
                input
                    .get("path")
                    .or_else(|| input.get("file"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        })?;
    Some(relative_path(project_root, &path))
}

fn relative_path(root: Option<&Path>, path: &str) -> String {
    let path_buf = PathBuf::from(path);
    root.and_then(|root| path_buf.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

fn quoted_subject(tool: &UiToolItem, subject: Option<&str>) -> String {
    tool.input
        .as_ref()
        .and_then(|input| input.get("query").and_then(Value::as_str))
        .map(|query| format!("“{}”", sanitize_bounded(query, 80)))
        .or_else(|| subject.map(str::to_owned))
        .unwrap_or_else(|| "files".to_owned())
}

fn execute_subject(tool: &UiToolItem) -> String {
    let Some(input) = &tool.input else {
        return tool.title.clone();
    };
    input
        .get("argv")
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(|arg| sanitize_bounded(arg, 80))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|command| !command.is_empty())
        .or_else(|| {
            input
                .get("command")
                .and_then(Value::as_str)
                .map(|value| sanitize_bounded(value, 120))
        })
        .unwrap_or_else(|| tool.title.clone())
}

fn status_verb(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending | ToolStatus::Running => "Running",
        ToolStatus::Completed => "✓",
        ToolStatus::Failed => "✗",
        ToolStatus::Declined => "!",
    }
}

fn detail(tool: &UiToolItem) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(input) = &tool.input {
        parts.push(format!(
            "input: {}",
            sanitize_bounded(&input.to_string(), 2_000)
        ));
    }
    if let Some(output) = &tool.output {
        parts.push(format!("output: {}", sanitize_bounded(output, 4_000)));
    }
    if let Some(error) = &tool.error {
        parts.push(format!("error: {}", sanitize_bounded(error, 2_000)));
    }
    if let Some(exit_code) = tool.exit_code {
        parts.push(format!("exit: {exit_code}"));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn diff_counts(diffs: &[coducktor_protocol::FileDiff]) -> (usize, usize, usize) {
    diffs.iter().fold((0, 0, 0), |(files, adds, dels), diff| {
        let old = diff.old_text.as_deref().unwrap_or_default().lines().count();
        let new = diff.new_text.as_deref().unwrap_or_default().lines().count();
        (
            files + 1,
            adds + new.saturating_sub(old),
            dels + old.saturating_sub(new),
        )
    })
}

fn sanitize_bounded(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut escape = false;
    for character in value.chars() {
        if escape {
            if character.is_ascii_alphabetic() {
                escape = false;
            }
            continue;
        }
        if character == '\u{1b}' {
            escape = true;
            continue;
        }
        if character.is_control() && character != '\n' && character != '\t' {
            continue;
        }
        output.push(character);
        if output.chars().count() >= max_chars {
            output.push('…');
            break;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(kind: ToolKind, input: Value) -> UiToolItem {
        UiToolItem {
            id: "tool-1".to_owned(),
            name: "runner".to_owned(),
            tool_kind: kind,
            title: "runner".to_owned(),
            status: ToolStatus::Completed,
            input: Some(input),
            output: None,
            error: None,
            diffs: None,
            locations: None,
            exit_code: None,
            parent_item_id: None,
        }
    }

    #[test]
    fn execute_uses_structured_argv_and_keeps_failure_detail() {
        let mut item = tool(ToolKind::Execute, json!({"argv": ["cargo", "test"]}));
        item.status = ToolStatus::Failed;
        item.exit_code = Some(1.0);
        item.error = Some("bad\u{1b}[31m output".to_owned());
        let presentation = present_tool(&item, None);
        assert_eq!(presentation.title, "✗ cargo test");
        assert_eq!(presentation.status, ToolStatus::Failed);
        assert!(
            presentation
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("exit: 1"))
        );
        assert!(
            !presentation
                .preview
                .as_deref()
                .unwrap_or_default()
                .contains('\u{1b}')
        );
    }

    #[test]
    fn read_paths_are_project_relative_when_possible() {
        let mut item = tool(ToolKind::Read, json!({}));
        item.locations = Some(vec![coducktor_protocol::ToolLocation {
            path: "/repo/src/lib.rs".to_owned(),
            line: Some(4.0),
        }]);
        assert_eq!(
            present_tool(&item, Some(Path::new("/repo"))).title,
            "Read src/lib.rs"
        );
    }

    #[test]
    fn unknown_tools_keep_a_bounded_fallback() {
        let mut item = tool(ToolKind::Other, json!({"payload": "x"}));
        item.name = "mystery".to_owned();
        item.output = Some("x".repeat(3_000));
        let presentation = present_tool(&item, None);
        assert_eq!(presentation.title, "mystery");
        assert!(
            presentation
                .preview
                .as_deref()
                .unwrap_or_default()
                .chars()
                .count()
                <= 241
        );
    }
}
