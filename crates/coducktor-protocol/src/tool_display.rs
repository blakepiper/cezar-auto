use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui_events::ToolKind;

const MAX_LABEL: usize = 120;

/// The display model computed once for every tool item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDisplay {
    pub tool_kind: ToolKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
}

/// Pure, panic-free tool-display mapping.
pub fn tool_display(name: &str, input: Option<&Value>) -> ToolDisplay {
    let key = name.to_ascii_lowercase();

    match key.as_str() {
        "bash" | "commandexecution" => ToolDisplay {
            tool_kind: ToolKind::Execute,
            title: titled("Ran", command_text(input)),
            subtitle: field(input, &["description"]),
        },
        "edit" | "multiedit" => ToolDisplay {
            tool_kind: ToolKind::Edit,
            title: titled("Edit", field(input, &["file_path", "filePath", "path"])),
            subtitle: None,
        },
        "write" => ToolDisplay {
            tool_kind: ToolKind::Edit,
            title: titled("Write", field(input, &["file_path", "filePath", "path"])),
            subtitle: None,
        },
        "notebookedit" => ToolDisplay {
            tool_kind: ToolKind::Edit,
            title: titled(
                "Edit",
                field(
                    input,
                    &[
                        "notebook_path",
                        "notebookPath",
                        "file_path",
                        "filePath",
                        "path",
                    ],
                ),
            ),
            subtitle: None,
        },
        "filechange" => {
            let paths = change_paths(input);
            let label = match paths.len() {
                1 => paths.first().cloned(),
                length if length > 1 => Some(format!("{length} files")),
                _ => None,
            };
            ToolDisplay {
                tool_kind: ToolKind::Edit,
                title: titled("Edit", label),
                subtitle: None,
            }
        }
        "read" => ToolDisplay {
            tool_kind: ToolKind::Read,
            title: titled("Read", field(input, &["file_path", "filePath", "path"])),
            subtitle: None,
        },
        "imageview" => ToolDisplay {
            tool_kind: ToolKind::Read,
            title: titled("View image", field(input, &["path"])),
            subtitle: None,
        },
        "glob" | "grep" => ToolDisplay {
            tool_kind: ToolKind::Search,
            title: titled("Search", field(input, &["pattern", "query"])),
            subtitle: field(input, &["path"]),
        },
        "webfetch" => ToolDisplay {
            tool_kind: ToolKind::Fetch,
            title: titled("Fetch", field(input, &["url"])),
            subtitle: None,
        },
        "websearch" => ToolDisplay {
            tool_kind: ToolKind::Fetch,
            title: titled("Web search", field(input, &["query"])),
            subtitle: None,
        },
        "task" | "agent" => {
            let verb = if key == "agent" { "Agent" } else { "Task" };
            let subagent = field(input, &["subagent_type", "subagentType"]);
            let label = field(input, &["description", "prompt"]).or_else(|| subagent.clone());
            ToolDisplay {
                tool_kind: ToolKind::Task,
                title: match label.as_deref() {
                    Some(label) => format!("{verb}: {label}"),
                    None => verb.to_owned(),
                },
                subtitle: if label == subagent { None } else { subagent },
            }
        }
        "skill" => {
            let skill = field(input, &["skill", "name", "command"]);
            ToolDisplay {
                tool_kind: ToolKind::Task,
                title: skill
                    .map(|value| format!("Skill: {value}"))
                    .unwrap_or_else(|| "Skill".to_owned()),
                subtitle: field(input, &["args", "description"]),
            }
        }
        "todowrite" | "todolist" | "plan" | "taskcreate" | "taskupdate" | "tasklist" => {
            ToolDisplay {
                tool_kind: ToolKind::Plan,
                title: "Update plan".to_owned(),
                subtitle: None,
            }
        }
        "contextcompaction" => ToolDisplay {
            tool_kind: ToolKind::Other,
            title: "Compacted context".to_owned(),
            subtitle: None,
        },
        "mcptoolcall" => {
            let server = field(input, &["server"]);
            let tool = field(input, &["tool"]);
            let has_pair = server.is_some() && tool.is_some();
            ToolDisplay {
                tool_kind: ToolKind::Other,
                title: match (server, tool) {
                    (Some(server), Some(tool)) => format!("{server}.{tool}"),
                    _ => "MCP tool".to_owned(),
                },
                subtitle: if has_pair {
                    None
                } else {
                    heuristic_label(input)
                },
            }
        }
        _ if key.starts_with("mcp__") => {
            let parts: Vec<&str> = name.split("__").filter(|part| !part.is_empty()).collect();
            if parts.len() >= 3 {
                return ToolDisplay {
                    tool_kind: ToolKind::Other,
                    title: format!("{}.{}", parts[1], parts[2..].join("__")),
                    subtitle: None,
                };
            }
            unknown_tool(name, input)
        }
        _ => unknown_tool(name, input),
    }
}

fn unknown_tool(name: &str, input: Option<&Value>) -> ToolDisplay {
    ToolDisplay {
        tool_kind: ToolKind::Other,
        title: if name.trim().is_empty() {
            "Tool".to_owned()
        } else {
            one_line(name)
        },
        subtitle: heuristic_label(input),
    }
}

fn is_record(value: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    value.and_then(Value::as_object)
}

fn one_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() > MAX_LABEL {
        chars[..MAX_LABEL - 1].iter().collect::<String>() + "…"
    } else {
        collapsed
    }
}

fn field(input: Option<&Value>, keys: &[&str]) -> Option<String> {
    let record = is_record(input)?;
    keys.iter().find_map(|key| {
        record.get(*key).and_then(Value::as_str).and_then(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(one_line(value))
            }
        })
    })
}

fn command_text(input: Option<&Value>) -> Option<String> {
    let record = is_record(input)?;
    match record.get("command") {
        Some(Value::String(command)) if !command.trim().is_empty() => Some(one_line(command)),
        Some(Value::Array(command)) => {
            let argv: Vec<&str> = command.iter().filter_map(Value::as_str).collect();
            (!argv.is_empty()).then(|| one_line(&argv.join(" ")))
        }
        _ => None,
    }
}

fn change_paths(input: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(changes)) = is_record(input).and_then(|record| record.get("changes"))
    else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn heuristic_label(input: Option<&Value>) -> Option<String> {
    field(
        input,
        &[
            "description",
            "query",
            "url",
            "filePath",
            "file_path",
            "path",
            "pattern",
            "name",
        ],
    )
}

fn titled(verb: &str, label: Option<String>) -> String {
    label.map_or_else(|| verb.to_owned(), |label| format!("{verb} {label}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_tool_display_cases_match_the_node_protocol() {
        let cases = [
            (
                "Bash",
                Some(serde_json::json!({"command":"npm test","description":"Run the unit tests"})),
                ToolDisplay {
                    tool_kind: ToolKind::Execute,
                    title: "Ran npm test".into(),
                    subtitle: Some("Run the unit tests".into()),
                },
            ),
            (
                "fileChange",
                Some(
                    serde_json::json!({"changes":[{"path":"src/a.ts"},{"path":"src/b.ts"},{"path":"src/c.ts"}]}),
                ),
                ToolDisplay {
                    tool_kind: ToolKind::Edit,
                    title: "Edit 3 files".into(),
                    subtitle: None,
                },
            ),
            (
                "Task",
                Some(
                    serde_json::json!({"description":"Explore the repo","subagent_type":"Explore"}),
                ),
                ToolDisplay {
                    tool_kind: ToolKind::Task,
                    title: "Task: Explore the repo".into(),
                    subtitle: Some("Explore".into()),
                },
            ),
            (
                "mcp__github__list_prs",
                Some(serde_json::json!({"state":"open"})),
                ToolDisplay {
                    tool_kind: ToolKind::Other,
                    title: "github.list_prs".into(),
                    subtitle: None,
                },
            ),
        ];

        for (name, input, expected) in cases {
            assert_eq!(tool_display(name, input.as_ref()), expected);
        }
    }

    #[test]
    fn malformed_input_never_panics() {
        for input in [
            None,
            Some(Value::Null),
            Some(Value::from(42)),
            Some(Value::Array(vec![])),
        ] {
            assert!(!tool_display("Bash", input.as_ref()).title.is_empty());
            assert!(!tool_display("", input.as_ref()).title.is_empty());
        }
    }
}
