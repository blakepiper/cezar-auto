//! `<dataDir>/runs/<id>.ndjson` — the append-only per-run event log. This module owns raw NDJSON
//! I/O and sequence-number bookkeeping; callers provide already-normalized [`RunEvent`] values.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use coducktor_contract::events::RunEvent;

/// `<dataDir>/runs/<runId>.ndjson`.
pub fn events_path(data_dir: &Path, run_id: &str) -> PathBuf {
    data_dir.join("runs").join(format!("{run_id}.ndjson"))
}

/// Read every persisted event for a run, oldest first. A
/// missing file reads as no events, and an unparseable LINE is skipped rather than failing
/// the whole read — one truncated line (e.g. a write cut short by a crash) must not hide
/// every event before it.
pub fn read_events(path: &Path) -> Vec<RunEvent> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Whether the durable log ends with a structured question that has not received a user reply.
/// Used only when upgrading legacy `waiting` records, whose status predated the idle/needs-input
/// split. A later user message resolves the latest question just as the thread reducer does.
pub fn has_pending_ask(path: &Path) -> bool {
    let mut pending = false;
    for event in read_events(path) {
        match event.event_type.as_str() {
            "ask.requested" => pending = true,
            "user-message" if pending => pending = false,
            _ => {}
        }
    }
    pending
}

/// Append one already-sequenced, already-redacted event line. The
/// synchronous append — local NDJSON appends at agent-event rates are effectively free, so
/// local append is synchronous and creates `<dataDir>/runs/` on first use.
pub fn append_event(path: &Path, event: &RunEvent) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(event).map_err(io::Error::other)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

/// A run-scoped append handle. `RunManager` retains one while a run is open, avoiding an
/// open/close pair for every streamed delta. Each complete NDJSON line is flushed before it is
/// announced to observers so the durable log remains the source of truth after a crash.
pub struct BufferedEventAppender {
    writer: BufWriter<fs::File>,
}

impl BufferedEventAppender {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn append(&mut self, event: &RunEvent) -> io::Result<()> {
        let line = serde_json::to_string(event).map_err(io::Error::other)?;
        writeln!(self.writer, "{line}")?;
        self.writer.flush()
    }
}

/// The highest `seq` persisted so far, or `0` for an empty/missing log. Mirrors
/// The one file read a restarted process needs before its first
/// post-restart append, so a fresh in-memory counter cannot collide with `seq`s a client
/// already replayed (the frozen-transcript symptom class of #424).
pub fn rehydrate_seq(path: &Path) -> f64 {
    read_events(path)
        .iter()
        .fold(0.0_f64, |max, event| max.max(event.seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(seq: f64, event_type: &str) -> RunEvent {
        let mut extra = serde_json::Map::new();
        extra.insert("text".to_owned(), json!(format!("line {seq}")));
        RunEvent {
            seq,
            ts: "2026-01-01T00:00:00.000Z".into(),
            step_id: Some("task".into()),
            event_type: event_type.into(),
            extra,
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_events() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_events(&events_path(dir.path(), "none")).is_empty());
    }

    #[test]
    fn append_then_read_round_trips_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "r1");
        append_event(&path, &event(1.0, "message")).unwrap();
        append_event(&path, &event(2.0, "message")).unwrap();
        let events = read_events(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1.0);
        assert_eq!(events[1].seq, 2.0);
        assert_eq!(
            events[1].extra.get("text").and_then(|v| v.as_str()),
            Some("line 2")
        );
    }

    #[test]
    fn a_truncated_trailing_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "r1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let good = serde_json::to_string(&event(1.0, "message")).unwrap();
        fs::write(&path, format!("{good}\n{{\"seq\":2,\"ts\":")).unwrap();
        let events = read_events(&path);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1.0);
    }

    #[test]
    fn rehydrate_seq_is_zero_for_an_empty_log() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(rehydrate_seq(&events_path(dir.path(), "none")), 0.0);
    }

    #[test]
    fn rehydrate_seq_finds_the_max_across_all_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "r1");
        append_event(&path, &event(1.0, "message")).unwrap();
        append_event(&path, &event(5.0, "message")).unwrap();
        append_event(&path, &event(3.0, "message")).unwrap();
        assert_eq!(rehydrate_seq(&path), 5.0);
    }

    #[test]
    fn pending_ask_tracks_the_latest_question_and_user_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "r1");
        append_event(&path, &event(1.0, "ask.requested")).unwrap();
        assert!(has_pending_ask(&path));
        append_event(&path, &event(2.0, "user-message")).unwrap();
        assert!(!has_pending_ask(&path));
    }
}
