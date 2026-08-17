//! The global follow-up inbox: `.ai/coducktor/todos.json`, a flat JSON array agents
//! append to (via `DUCK_TODOS_FILE`). Agent entries are external data — each one is
//! validated on read and malformed ones are skipped, never fatal. Writes land atomically
//! (tmp + rename, the `runs::store` pattern).
//!
//! The terminal reads this file when refreshing the inbox; this synchronous file layer does
//! not maintain a filesystem watcher.
//!
//! Callers that perform concurrent writes are responsible for serializing them. Each write still
//! uses the same read-modify-write and atomic-rename rules as the other durable state files.

use std::fs;
use std::path::{Path, PathBuf};

use coducktor_contract::skills::TodoItem;

pub fn todos_path(data_dir: &Path) -> PathBuf {
    data_dir.join("todos.json")
}

struct RawRead {
    items: Vec<TodoItem>,
    needs_rewrite: bool,
}

/// Parse + validate the file. Broken JSON / non-array → `[]`; bad entries are skipped;
/// entries without an id get one assigned (a monotonically-increasing placeholder here —
/// see [`read_todos`]'s doc for why a random UUID isn't needed).
fn read_raw(data_dir: &Path) -> RawRead {
    let Ok(raw) = fs::read_to_string(todos_path(data_dir)) else {
        return RawRead {
            items: Vec::new(),
            needs_rewrite: false,
        }; // no file yet — empty inbox
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return RawRead {
            items: Vec::new(),
            needs_rewrite: false,
        };
    };
    let Some(array) = parsed.as_array() else {
        return RawRead {
            items: Vec::new(),
            needs_rewrite: false,
        };
    };

    let mut items = Vec::new();
    let mut needs_rewrite = false;
    for entry in array {
        let Some(mut item) = parse_todo_entry(entry) else {
            continue;
        };
        if item.id.is_empty() {
            // Agent entries arrive without ids — assign one so the GUI can address the
            // entry; the file is rewritten (by the caller) on this read.
            needs_rewrite = true;
            item.id = new_id();
        }
        items.push(item);
    }
    RawRead {
        items,
        needs_rewrite,
    }
}

/// `todoSchema.safeParse`: `summary` (non-empty) is the one truly required field; every
/// other field is a plain `.optional()` with no `.catch()`, meaning a PRESENT-but-wrong-typed
/// value fails the whole entry, exactly what `#[derive(Deserialize)]` on
/// [`TodoItem`] already does — so this hands the (summary-checked) value straight to that
/// derive. `id` is `z.string().min(1).optional()`, which needs its own case split: ABSENT is
/// fine (agent entries arrive with no `id` key at all), but a PRESENT id must still be a
/// non-empty string, same as any other field with no `.catch()`. `coducktor_contract`'s
/// `TodoItem::id` is a plain required `String` (every OTHER consumer of a todo item wants
/// one in hand), so an absent key is stood in with `""` here so the derive succeeds, and
/// [`read_raw`]'s existing "assign + persist" step fills in a real one from that sentinel.
fn parse_todo_entry(value: &serde_json::Value) -> Option<TodoItem> {
    let object = value.as_object()?;
    let summary = object.get("summary")?.as_str()?;
    if summary.is_empty() {
        return None;
    }
    let mut object = object.clone();
    match object.get("id") {
        None => {
            object.insert("id".to_owned(), serde_json::json!(""));
        }
        Some(serde_json::Value::String(s)) if !s.is_empty() => {} // keep as-is
        Some(_) => return None, // present but empty or the wrong type — z.string().min(1) fails
    }
    serde_json::from_value(serde_json::Value::Object(object)).ok()
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("todo-{nanos:x}-{n:x}")
}

fn write_atomic(data_dir: &Path, items: &[TodoItem]) -> std::io::Result<()> {
    let file = todos_path(data_dir);
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp_path = PathBuf::from(tmp);
    fs::create_dir_all(data_dir)?;
    fs::write(
        &tmp_path,
        serde_json::to_string_pretty(items).expect("TodoItem always serializes"),
    )?;
    fs::rename(&tmp_path, &file)
}

/// Reads the inbox, assigning ids to any legacy agent-written entries missing one and
/// persisting that assignment (best-effort — a read must never fail because the rewrite
/// couldn't be saved).
pub fn read_todos(data_dir: &Path) -> Vec<TodoItem> {
    let RawRead {
        items,
        needs_rewrite,
    } = read_raw(data_dir);
    if needs_rewrite {
        let _ = write_atomic(data_dir, &items);
    }
    items
}

/// Check off (delete) an entry. `false` when the id isn't there.
pub fn remove_todo(data_dir: &Path, id: &str) -> std::io::Result<bool> {
    let RawRead { items, .. } = read_raw(data_dir);
    let before = items.len();
    let next: Vec<TodoItem> = items.into_iter().filter(|t| t.id != id).collect();
    if next.len() == before {
        return Ok(false);
    }
    write_atomic(data_dir, &next)?;
    Ok(true)
}

/// The task text "▶ Run" turns an entry into: the suggested prompt (or the summary when the
/// entry carries none), plus the suggested args as a trailing line.
pub fn todo_task_text(
    summary: &str,
    suggested_prompt: Option<&str>,
    suggested_args: Option<&str>,
) -> String {
    let base = suggested_prompt.unwrap_or(summary).trim();
    let mut task = if base.is_empty() {
        summary.to_owned()
    } else {
        base.to_owned()
    };
    if let Some(args) = suggested_args {
        task.push_str(&format!("\n\nArguments: {args}"));
    }
    task
}

/// Record that "▶ Run" turned the entry into task `task_id`. The entry stays in the file as
/// an audit trail. First start wins: an entry that already carries a `started_task_id` is
/// left untouched and answers `false`.
pub fn mark_started(data_dir: &Path, id: &str, task_id: &str) -> std::io::Result<bool> {
    let RawRead { mut items, .. } = read_raw(data_dir);
    let Some(item) = items.iter_mut().find(|t| t.id == id) else {
        return Ok(false);
    };
    if item.started_task_id.is_some() {
        return Ok(false);
    }
    item.started_task_id = Some(task_id.to_owned());
    write_atomic(data_dir, &items)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_raw(dir: &Path, value: serde_json::Value) {
        fs::create_dir_all(dir).unwrap();
        fs::write(todos_path(dir), serde_json::to_string(&value).unwrap()).unwrap();
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_inbox() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_todos(dir.path()).is_empty());
    }

    #[test]
    fn malformed_json_and_non_array_json_both_read_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(todos_path(dir.path()), "not json").unwrap();
        assert!(read_todos(dir.path()).is_empty());

        write_raw(dir.path(), json!({ "not": "an array" }));
        assert!(read_todos(dir.path()).is_empty());
    }

    #[test]
    fn a_malformed_entry_is_skipped_without_dropping_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(
            dir.path(),
            json!([
                { "id": "ok", "summary": "a real follow-up" },
                { "id": "bad", "summary": "" }, // min(1) fails
                { "id": "bad2" }, // summary missing entirely
                "not even an object",
            ]),
        );
        let items = read_todos(dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "ok");
    }

    #[test]
    fn an_entry_without_an_id_is_assigned_one_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), json!([{ "summary": "from an agent" }]));
        let items = read_todos(dir.path());
        assert_eq!(items.len(), 1);
        assert!(!items[0].id.is_empty());

        // The rewrite persisted — re-reading finds the SAME id, not a fresh one.
        let items_again = read_todos(dir.path());
        assert_eq!(items_again[0].id, items[0].id);
    }

    #[test]
    fn a_present_but_empty_id_fails_the_whole_entry_unlike_an_absent_one() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(
            dir.path(),
            json!([
                { "id": "", "summary": "explicit empty id" },
                { "id": 5, "summary": "wrong type" },
                { "summary": "no id at all — this one survives" },
            ]),
        );
        let items = read_todos(dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary, "no id at all — this one survives");
    }

    #[test]
    fn remove_todo_deletes_the_matching_entry_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(
            dir.path(),
            json!([
                { "id": "a", "summary": "keep" },
                { "id": "b", "summary": "remove me" },
            ]),
        );
        assert!(remove_todo(dir.path(), "b").unwrap());
        let items = read_todos(dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "a");
        assert!(!remove_todo(dir.path(), "b").unwrap(), "already gone");
    }

    #[test]
    fn mark_started_sets_the_task_id_once_and_never_overwrites_it() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), json!([{ "id": "a", "summary": "run me" }]));
        assert!(mark_started(dir.path(), "a", "task-1").unwrap());
        assert_eq!(
            read_todos(dir.path())[0].started_task_id.as_deref(),
            Some("task-1")
        );
        assert!(
            !mark_started(dir.path(), "a", "task-2").unwrap(),
            "first start wins"
        );
        assert_eq!(
            read_todos(dir.path())[0].started_task_id.as_deref(),
            Some("task-1")
        );
        assert!(!mark_started(dir.path(), "missing", "task-3").unwrap());
    }

    #[test]
    fn todo_task_text_prefers_the_suggested_prompt_and_appends_args() {
        assert_eq!(
            todo_task_text("the summary", Some("do the specific thing"), None),
            "do the specific thing"
        );
        assert_eq!(todo_task_text("the summary", None, None), "the summary");
        assert_eq!(
            todo_task_text("the summary", None, Some("--flag")),
            "the summary\n\nArguments: --flag"
        );
        assert_eq!(
            todo_task_text("the summary", Some("   "), None),
            "the summary",
            "a blank suggested prompt falls back to the summary"
        );
    }
}
