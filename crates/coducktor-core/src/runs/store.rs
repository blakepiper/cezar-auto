//! `<project-state>/runs.json` — the run index. This module loads, reconciles, and
//! atomically saves the array of [`RunRecord`] values. Run orchestration, event fan-out, and
//! redaction belong to the workflow manager.
//!
//! `RunRecord` itself is defined in `coducktor_contract::runs`; this persistence layer adds
//! the normalization rules that a plain `#[derive(Deserialize)]` cannot express — see
//! [`normalize_run_record_value`].

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use coducktor_contract::runs::RunRecord;
use coducktor_contract::{RunStatus, StepStatus};

use crate::time::{is_zod_datetime, now_iso8601};
use crate::workspace::config::atomic_tmp_path;

/// The legacy
/// spelling of `claude`, still accepted on the way IN and folded to `claude` on the way
/// through, so an old `runs.json` stays parseable without a fourth runner id ever reaching a
/// consumer.
const LEGACY_CLAUDE_CLI: &str = "claude-cli";

/// Interrupted-run error text stored on reconciled runs.
const INTERRUPTED_ERROR: &str = "interrupted — coducktor process exited during the run";

pub const MAX_RUNS_KEPT: usize = 300;
pub const MAX_ARCHIVED_KEPT: usize = 500;

/// `<dataDir>/runs.json`.
pub fn index_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runs.json")
}

/// Read `runs.json` on demand — never cached. `keep_live` retains active records instead of
/// marking them interrupted; see [`reconcile_loaded_run`].
///
/// Missing, valid, partially salvageable, and corrupt files are distinguished by
/// [`load_run_index_outcome`]. Valid siblings survive one malformed entry, while callers can
/// quarantine writes so the original bytes are never silently replaced by the salvaged subset.
#[derive(Debug)]
pub enum RunIndexLoad {
    Missing,
    Valid(Vec<RunRecord>),
    ValidWithSalvage(Vec<RunRecord>),
    Corrupt,
}

impl RunIndexLoad {
    pub fn records(&self) -> &[RunRecord] {
        match self {
            Self::Valid(records) | Self::ValidWithSalvage(records) => records,
            Self::Missing | Self::Corrupt => &[],
        }
    }

    pub fn write_quarantined(&self) -> bool {
        matches!(self, Self::ValidWithSalvage(_) | Self::Corrupt)
    }
}

pub fn load_run_index_outcome(index_path: &Path, keep_live: bool) -> RunIndexLoad {
    let raw = match fs::read_to_string(index_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return RunIndexLoad::Missing,
        Err(_) => return RunIndexLoad::Corrupt,
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return RunIndexLoad::Corrupt;
    };
    let Some(array) = value.as_array() else {
        return RunIndexLoad::Corrupt;
    };

    let mut records = Vec::with_capacity(array.len());
    let mut salvaged = false;
    for item in array {
        match try_parse_run_record(item.clone()) {
            Some(record) => records.push(reconcile_loaded_run(record, keep_live)),
            None => salvaged = true,
        }
    }
    if salvaged {
        RunIndexLoad::ValidWithSalvage(records)
    } else {
        RunIndexLoad::Valid(records)
    }
}

pub fn load_run_index(index_path: &Path, keep_live: bool) -> Vec<RunRecord> {
    load_run_index_outcome(index_path, keep_live)
        .records()
        .to_vec()
}

/// Every run currently in `runs`, newest-`createdAt`-first — mirrors `RunStore.listRuns`.
pub fn list_runs_by_recency(runs: &[RunRecord]) -> Vec<&RunRecord> {
    let mut sorted: Vec<&RunRecord> = runs.iter().collect();
    sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    sorted
}

/// Atomic owner-only write of the whole index through a collision-safe staging file. File data is
/// synced before rename and the containing directory is synced best-effort after rename.
pub fn write_run_index(index_path: &Path, runs: &[RunRecord]) -> io::Result<()> {
    write_run_index_with_hooks(index_path, runs, |_| Ok(()), |_| directory_sync(index_path))
}

/// The staging boundary is isolated so the crash/failure invariant can be tested without
/// depending on platform-specific permission or disk-full behavior: until the rename succeeds,
/// the prior index remains authoritative. `after_rename` isolates the same invariant on the other
/// side of it — the rename has already durably committed the new index, so nothing past that
/// point (directory-sync durability hardening) may turn a successful write into a reported
/// failure; production wires the real best-effort sync, a test can wire an injected one instead.
fn write_run_index_with_hooks(
    index_path: &Path,
    runs: &[RunRecord],
    before_rename: impl FnOnce(&Path) -> io::Result<()>,
    after_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let sorted = list_runs_by_recency(runs);
    let json = serde_json::to_vec_pretty(&sorted).map_err(io::Error::other)?;
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = atomic_tmp_path(index_path);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
        before_rename(&tmp_path)?;
        fs::rename(&tmp_path, index_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(index_path, fs::Permissions::from_mode(0o600))?;
        }
        // Best-effort: the rename above already committed the new index durably as far as the
        // filesystem's own ordering guarantees go. A failure here only means the directory
        // entry's own durability hardening did not happen, not that the write failed.
        let _ = after_rename(index_path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn directory_sync(index_path: &Path) -> io::Result<()> {
    if let Some(parent) = index_path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Explicitly repair a quarantined index. The original bytes are first copied to an owner-only,
/// collision-safe backup beside the index; only then is the supplied salvaged record set written.
/// Callers must opt in after presenting the backup location to the user.
pub fn backup_then_repair_run_index(index_path: &Path, runs: &[RunRecord]) -> io::Result<PathBuf> {
    backup_then_repair_run_index_with_writer(index_path, runs, write_run_index)
}

fn backup_then_repair_run_index_with_writer(
    index_path: &Path,
    runs: &[RunRecord],
    write_index: impl FnOnce(&Path, &[RunRecord]) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let staged_backup = atomic_tmp_path(index_path);
    let mut backup_name = staged_backup.file_name().unwrap_or_default().to_os_string();
    backup_name.push(".corrupt-backup.json");
    let backup_path = index_path.with_file_name(backup_name);
    fs::copy(index_path, &staged_backup)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&staged_backup, fs::Permissions::from_mode(0o600))?;
    }
    if let Err(error) = fs::rename(&staged_backup, &backup_path) {
        let _ = fs::remove_file(&staged_backup);
        return Err(error);
    }
    write_index(index_path, runs)?;
    Ok(backup_path)
}

/// Reconcile one record just read off disk with the fact that whichever process wrote it is
/// gone. This is shared by the stateful store's loader and the read-only workspace-index reader,
/// and the two must never diverge on what a `running` row on disk means.
///
/// `keep_live` (#367): leave `queued`/`running`/`waiting` untouched so a caller with a
/// `RunManager` can recover them. Everywhere else — one-shot CLI paths, and the read-only
/// index reader, which has no manager to recover into — a live-looking run is marked failed
/// so no ghost stays behind.
pub fn reconcile_loaded_run(mut run: RunRecord, keep_live: bool) -> RunRecord {
    if !keep_live && is_live_status(run.status) {
        run.status = RunStatus::Failed;
        run.error = Some(INTERRUPTED_ERROR.to_owned());
        run.finished_at.get_or_insert_with(now_iso8601);
        for step in &mut run.steps {
            if matches!(step.status, StepStatus::Running | StepStatus::Waiting) {
                step.status = StepStatus::Failed;
            }
        }
    }
    if !matches!(
        run.status,
        RunStatus::Running | RunStatus::Waiting | RunStatus::Queued
    ) {
        run.activity = None;
        run.monitoring_wake_at = None;
    }
    if run.status != RunStatus::Failed {
        run.auto_resume_at = None;
    }
    run.monitoring_wake_cap_reached = None;
    run
}

fn is_live_status(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Running | RunStatus::Queued | RunStatus::Waiting
    )
}

/// Ids of runs beyond the count-based retention budget — mirrors `RunStore`'s private
/// `pruneOldRuns`: keep the [`MAX_RUNS_KEPT`] most-recently-created non-archived runs and
/// the [`MAX_ARCHIVED_KEPT`] most-recently-created archived ones, list the rest. Pure
/// selection only; deleting the stale runs' index entries and on-disk files (NDJSON,
/// handoff journal, image dir) is the caller's job, same split as
/// `retention::select_reclaimable_worktrees` keeps from its own I/O enforcer.
pub fn select_stale_run_ids(runs: &[RunRecord]) -> Vec<String> {
    let sorted = list_runs_by_recency(runs);
    let mut stale: Vec<String> = sorted
        .iter()
        .filter(|r| !r.archived)
        .skip(MAX_RUNS_KEPT)
        .map(|r| r.id.clone())
        .collect();
    stale.extend(
        sorted
            .iter()
            .filter(|r| r.archived)
            .skip(MAX_ARCHIVED_KEPT)
            .map(|r| r.id.clone()),
    );
    stale
}

/// Applies `runRecordSchema`'s zod behaviors a plain `#[derive(Deserialize)]` on
/// `coducktor_contract::runs::RunRecord` cannot express, then hands the normalized value to
/// that derive for everything else (which is the overwhelming majority of the schema: most
/// fields here are either required, or `.optional()` with NO `.catch()` — meaning a
/// present-but-wrong-typed value is supposed to fail the WHOLE record, exactly what a plain
/// derive already does). Returns `None` when the value cannot become a valid record at all —
/// the caller (`load_run_index`) treats that as "abort loading the whole array", matching
/// `z.array(runRecordSchema).safeParse` failing on any one element.
///
/// The zod behaviors requiring help:
/// - `archived: z.boolean().default(false)` — absent key defaults; wrong type still fails.
///   [`RunRecord::archived`] has no serde default, so the absent case needs a real insert.
/// - `runner` / `steps[].backend`: `storedRunnerSchema` folds the legacy `claude-cli` id to
///   `claude` (#547) — the plain `Runner` enum has no such variant to fold.
/// - `monitoringWakeAt` / `autoResumeAt`: `z.string().datetime().optional().catch(undefined)`
///   — an invalid FORMAT (not just wrong JSON type) degrades to absent rather than failing.
/// - `autoResumeAttempts`: `z.number().int().min(0).optional().catch(undefined)` — same
///   degrade-in-place, gated on integer-and-non-negative.
/// - `blockedReason.retryAt`: `.optional().catch(undefined)`, nested one level down — only
///   that field degrades; a bad `retryAt` must not take the surrounding `blockedReason` (or
///   the whole record) down with it.
/// - `workflowDef`: `workflowDefSchema.optional().catch(undefined)` — the whole nested
///   workflow definition degrades to absent rather than failing the record; a def that no
///   longer validates is meant to fall back to catalog resolution by name, not evict the run.
fn normalize_run_record_value(value: &mut Value) -> Option<()> {
    let object = value.as_object_mut()?;

    object.entry("archived").or_insert(Value::Bool(false));

    if let Some(runner) = object.get_mut("runner") {
        fold_legacy_runner(runner);
    }
    if let Some(Value::Array(steps)) = object.get_mut("steps") {
        for step in steps {
            if let Some(step_object) = step.as_object_mut()
                && let Some(backend) = step_object.get_mut("backend")
            {
                fold_legacy_runner(backend);
            }
        }
    }

    strip_unless(object, "monitoringWakeAt", |v| {
        v.as_str().is_some_and(is_zod_datetime)
    });
    strip_unless(object, "autoResumeAt", |v| {
        v.as_str().is_some_and(is_zod_datetime)
    });
    strip_unless(object, "autoResumeAttempts", |v| {
        v.as_f64().is_some_and(|n| n.fract() == 0.0 && n >= 0.0)
    });
    if let Some(Value::Object(blocked)) = object.get_mut("blockedReason") {
        strip_unless(blocked, "retryAt", Value::is_string);
    }
    strip_unless(object, "workflowDef", |v| {
        serde_json::from_value::<coducktor_contract::workflows::WorkflowDef>(v.clone()).is_ok()
    });

    Some(())
}

fn fold_legacy_runner(value: &mut Value) {
    if value.as_str() == Some(LEGACY_CLAUDE_CLI) {
        *value = Value::String("claude".to_owned());
    }
}

/// Removes `key` from `object` when present but not `valid`; leaves an absent key alone
/// (already reads as "not set" to the derive below) and a present-and-valid one untouched.
fn strip_unless(object: &mut Map<String, Value>, key: &str, valid: impl Fn(&Value) -> bool) {
    let keep = object.get(key).is_some_and(&valid);
    if !keep {
        object.remove(key);
    }
}

fn try_parse_run_record(mut value: Value) -> Option<RunRecord> {
    normalize_run_record_value(&mut value)?;
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use coducktor_contract::runs::StepState;
    use coducktor_contract::{Runner, StepKind};
    use serde_json::json;

    fn legacy_run(overrides: Value) -> Value {
        let mut base = json!({
            "id": "legacy-1",
            "title": "fix the login bug",
            "workflow": "quick-task",
            "task": "fix the login bug",
            "status": "done",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "tokensUsed": 0,
            "archived": false,
            "steps": [],
        });
        let base_obj = base.as_object_mut().unwrap();
        for (k, v) in overrides.as_object().unwrap() {
            base_obj.insert(k.clone(), v.clone());
        }
        base
    }

    fn write_index(dir: &std::path::Path, runs: Value) -> PathBuf {
        let path = index_path(dir);
        fs::write(&path, serde_json::to_string(&runs).unwrap()).unwrap();
        path
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_run_index(&index_path(dir.path()), false).is_empty());
    }

    #[test]
    fn a_malformed_json_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        fs::write(&path, "not json").unwrap();
        assert!(load_run_index(&path, false).is_empty());
    }

    #[test]
    fn one_invalid_record_salvages_its_sibling_and_quarantines_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([
                legacy_run(json!({})),
                legacy_run(json!({ "id": "bad-status", "status": "not-a-status" })),
            ]),
        );
        let outcome = load_run_index_outcome(&path, false);
        assert!(outcome.write_quarantined());
        assert_eq!(outcome.records().len(), 1);
        assert_eq!(outcome.records()[0].id, "legacy-1");
    }

    #[test]
    fn loads_a_record_carrying_claude_cli_and_folds_it_to_claude() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(json!({
                "runner": "claude-cli",
                "steps": [{
                    "id": "task", "name": "Do the task", "kind": "agent", "status": "done",
                    "iterations": 1, "tokensUsed": 0, "backend": "claude-cli",
                }],
            }))]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].runner, Some(Runner::Claude));
        assert_eq!(runs[0].steps[0].backend, Some(Runner::Claude));
    }

    #[test]
    fn does_not_let_one_claude_cli_record_evict_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([
                legacy_run(json!({ "id": "legacy-cli", "runner": "claude-cli" })),
                legacy_run(json!({ "id": "modern", "runner": "codex" })),
            ]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs.iter().find(|r| r.id == "legacy-cli").unwrap().runner,
            Some(Runner::Claude)
        );
        assert_eq!(
            runs.iter().find(|r| r.id == "modern").unwrap().runner,
            Some(Runner::Codex)
        );
    }

    #[test]
    fn salvages_a_malformed_wake_deadline_without_dropping_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(
                json!({ "activity": "monitoring", "monitoringWakeAt": "not-a-date" })
            )]),
        );
        // load with keep_live so status/activity survive long enough to assert the
        // deadline specifically got scrubbed rather than the record surviving by accident
        // of a live-status reconcile also clearing it.
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].monitoring_wake_at, None);
    }

    #[test]
    fn rejects_an_unknown_activity_value_at_the_schema_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(
                json!({ "status": "running", "activity": "bogus" })
            )]),
        );
        assert!(load_run_index(&path, false).is_empty());
    }

    #[test]
    fn archived_defaults_to_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let mut run = legacy_run(json!({}));
        run.as_object_mut().unwrap().remove("archived");
        let path = write_index(dir.path(), json!([run]));
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].archived);
    }

    #[test]
    fn a_present_but_wrong_typed_archived_fails_the_whole_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(json!({ "archived": "not-a-bool" }))]),
        );
        assert!(load_run_index(&path, false).is_empty());
    }

    #[test]
    fn negative_auto_resume_attempts_is_caught_to_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(
                json!({ "status": "failed", "autoResumeAttempts": -5 })
            )]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].auto_resume_attempts, None);
    }

    #[test]
    fn a_malformed_workflow_def_is_caught_to_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(
                json!({ "workflowDef": { "not": "a workflow" } })
            )]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].workflow_def.is_none());
    }

    #[test]
    fn a_valid_workflow_def_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(json!({ "workflowDef": {
                "name": "quick-task",
                "steps": [{ "id": "task", "prompt": "{{task}}" }],
                "source": "built-in",
            } }))]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].workflow_def.as_ref().unwrap().name, "quick-task");
    }

    #[test]
    fn blocked_reason_survives_a_malformed_retry_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(json!({
                "status": "queued",
                "blockedReason": {
                    "type": "provider_quota",
                    "providers": ["claude"],
                    "retryAt": 12345,
                },
            }))]),
        );
        let runs = load_run_index(&path, true);
        assert_eq!(runs.len(), 1);
        let blocked = runs[0].blocked_reason.as_ref().unwrap();
        assert_eq!(blocked.retry_at, None);
        assert_eq!(
            blocked.providers,
            vec![coducktor_contract::QuotaProvider::Claude]
        );
    }

    #[test]
    fn reconcile_marks_a_live_run_interrupted_unless_keep_live() {
        let mut run = RunRecord {
            id: "r1".into(),
            title: "t".into(),
            workflow: "w".into(),
            task: "t".into(),
            status: RunStatus::Running,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            steps: vec![StepState {
                id: "s".into(),
                name: "s".into(),
                kind: StepKind::Agent,
                status: StepStatus::Running,
                iterations: 1.0,
                tokens_used: 0.0,
                input_tokens: None,
                output_tokens: None,
                usage_invocations_started: None,
                usage_invocations_observed: None,
                usage_turns_started: None,
                usage_turns_recorded: None,
                usage_invocation_epoch: None,
                started_at: None,
                finished_at: None,
                error: None,
                session_id: None,
                backend: None,
                requested_runner: None,
                profile_id: None,
                reasoning_effort: None,
                cost_usd: None,
                model_identity: None,
                route_key: None,
                recovery_generation: None,
                routing_decision: None,
                extra: Map::new(),
            }],
            ..Default::default()
        };
        run = reconcile_loaded_run(run, false);
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.error.as_deref(), Some(INTERRUPTED_ERROR));
        assert!(run.finished_at.is_some());
        assert_eq!(run.steps[0].status, StepStatus::Failed);
    }

    #[test]
    fn reconcile_leaves_a_live_run_alone_when_keep_live() {
        let run = RunRecord {
            status: RunStatus::Running,
            ..Default::default()
        };
        let run = reconcile_loaded_run(run, true);
        assert_eq!(run.status, RunStatus::Running);
        assert!(run.error.is_none());
    }

    #[test]
    fn reconcile_clears_a_pending_auto_resume_on_a_non_failed_run() {
        let run = RunRecord {
            status: RunStatus::Done,
            auto_resume_at: Some("2026-01-01T00:00:00.000Z".into()),
            ..Default::default()
        };
        let run = reconcile_loaded_run(run, true);
        assert_eq!(run.auto_resume_at, None);
    }

    #[test]
    fn reconcile_keeps_a_pending_auto_resume_on_a_failed_run() {
        let run = RunRecord {
            status: RunStatus::Failed,
            auto_resume_at: Some("2026-01-01T00:00:00.000Z".into()),
            ..Default::default()
        };
        let run = reconcile_loaded_run(run, true);
        assert_eq!(
            run.auto_resume_at,
            Some("2026-01-01T00:00:00.000Z".to_owned())
        );
    }

    #[test]
    fn write_run_index_sorts_newest_created_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let older = RunRecord {
            id: "older".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        let newer = RunRecord {
            id: "newer".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            ..Default::default()
        };
        write_run_index(&path, &[older, newer]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.ends_with('\n'), "saveNow writes no trailing newline");
        let parsed: Vec<RunRecord> = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed[0].id, "newer");
        assert_eq!(parsed[1].id, "older");
    }

    #[test]
    fn write_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let run = RunRecord {
            id: "r1".into(),
            title: "hello".into(),
            status: RunStatus::Done,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        write_run_index(&path, std::slice::from_ref(&run)).unwrap();
        let loaded = load_run_index(&path, true);
        assert_eq!(loaded, vec![run]);
    }

    #[test]
    fn a_failure_before_rename_preserves_the_previous_index_and_cleans_its_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let previous = RunRecord {
            id: "previous".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        write_run_index(&path, std::slice::from_ref(&previous)).unwrap();
        let original = fs::read(&path).unwrap();

        let next = RunRecord {
            id: "next".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            ..Default::default()
        };
        let error = write_run_index_with_hooks(
            &path,
            &[next],
            |_| Err(io::Error::other("injected pre-rename failure")),
            |_| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), original);
        let staging_files = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(staging_files, 0);
    }

    #[test]
    fn unknown_run_and_step_keys_survive_a_read_modify_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_index(
            dir.path(),
            json!([legacy_run(json!({
                "futureRunField": {"kept": true},
                "steps": [{
                    "id": "task", "name": "Task", "kind": "agent", "status": "done",
                    "iterations": 1, "tokensUsed": 0, "futureStepField": [1, 2, 3]
                }]
            }))]),
        );
        let mut runs = load_run_index(&path, true);
        runs[0].title = "changed".to_owned();
        write_run_index(&path, &runs).unwrap();
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw[0]["futureRunField"], json!({"kept": true}));
        assert_eq!(raw[0]["steps"][0]["futureStepField"], json!([1, 2, 3]));
    }

    #[cfg(unix)]
    #[test]
    fn run_index_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        write_run_index(&path, &[]).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn select_stale_run_ids_keeps_the_newest_and_prunes_the_rest_per_bucket() {
        let mut runs = Vec::new();
        for i in 0..(MAX_RUNS_KEPT + 2) {
            runs.push(RunRecord {
                id: format!("live-{i}"),
                created_at: format!("2026-01-{:02}T00:00:00.000Z", (i % 28) + 1),
                archived: false,
                ..Default::default()
            });
        }
        for i in 0..(MAX_ARCHIVED_KEPT + 3) {
            runs.push(RunRecord {
                id: format!("archived-{i}"),
                created_at: format!("2026-02-{:02}T00:00:00.000Z", (i % 28) + 1),
                archived: true,
                ..Default::default()
            });
        }
        let stale = select_stale_run_ids(&runs);
        assert_eq!(stale.len(), 2 + 3);
        assert!(stale.iter().all(|id| runs.iter().any(|r| &r.id == id)));
    }

    #[test]
    fn explicit_repair_backs_up_corrupt_bytes_before_replacing_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let corrupt = b"{ definitely not json";
        fs::write(&path, corrupt).unwrap();

        let backup = backup_then_repair_run_index(&path, &[]).unwrap();

        assert_eq!(fs::read(backup).unwrap(), corrupt);
        assert_eq!(load_run_index(&path, true), Vec::<RunRecord>::new());
    }

    #[cfg(unix)]
    #[test]
    fn repair_backup_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        fs::write(&path, b"{ definitely not json").unwrap();

        let backup = backup_then_repair_run_index(&path, &[]).unwrap();

        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn failed_repair_replacement_preserves_the_corrupt_index_and_its_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let corrupt = b"{ definitely not json";
        fs::write(&path, corrupt).unwrap();

        let error = backup_then_repair_run_index_with_writer(&path, &[], |_, _| {
            Err(io::Error::other("injected repair replacement failure"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        let backups = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".corrupt-backup.json")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), corrupt);
    }

    /// R10: the disk-full instance of the same invariant
    /// `a_failure_before_rename_preserves_the_previous_index_and_cleans_its_staging_file` proves
    /// generically — pinned separately, with the specific `ErrorKind` a real out-of-space write
    /// would surface, since the fault matrix names it as its own scenario.
    #[test]
    fn disk_full_during_write_preserves_the_previous_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let previous = RunRecord {
            id: "previous".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        write_run_index(&path, std::slice::from_ref(&previous)).unwrap();
        let original = fs::read(&path).unwrap();

        let next = RunRecord {
            id: "next".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            ..Default::default()
        };
        let error = write_run_index_with_hooks(
            &path,
            std::slice::from_ref(&next),
            |_| Err(io::Error::from(io::ErrorKind::StorageFull)),
            |_| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    /// R10: the rename has already durably committed the new index by the time the best-effort
    /// directory sync runs, so a failure there must not be reported as a write failure, and must
    /// not leave the new content unreadable.
    #[test]
    fn a_directory_sync_failure_after_rename_does_not_fail_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let run = RunRecord {
            id: "run".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };

        write_run_index_with_hooks(
            &path,
            std::slice::from_ref(&run),
            |_| Ok(()),
            |_| Err(io::Error::other("injected directory-sync failure")),
        )
        .unwrap();

        assert_eq!(load_run_index(&path, true), vec![run]);
    }

    /// R10: `atomic_tmp_path` gives every writer its own collision-safe staging file, and rename
    /// is atomic, so two concurrent writers to the same index never interleave — the file on disk
    /// after both complete is always one writer's whole, valid content, never a corrupt mix.
    #[test]
    fn concurrent_writers_to_the_same_index_never_produce_corrupted_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let first = RunRecord {
            id: "first".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        let second = RunRecord {
            id: "second".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            ..Default::default()
        };

        let path_a = path.clone();
        let first_for_thread = first.clone();
        let handle_a = std::thread::spawn(move || {
            write_run_index(&path_a, std::slice::from_ref(&first_for_thread))
        });
        let path_b = path.clone();
        let second_for_thread = second.clone();
        let handle_b = std::thread::spawn(move || {
            write_run_index(&path_b, std::slice::from_ref(&second_for_thread))
        });
        handle_a.join().unwrap().unwrap();
        handle_b.join().unwrap().unwrap();

        let loaded = load_run_index(&path, true);
        assert_eq!(
            loaded.len(),
            1,
            "a torn/merged write would show as more or fewer records"
        );
        assert!(
            loaded[0] == first || loaded[0] == second,
            "the surviving record must be one writer's whole content, not a mix"
        );
    }

    /// R10: a write that cannot even create its staging file (no permission on the containing
    /// directory) must leave the previous index exactly as it was.
    #[cfg(unix)]
    #[test]
    fn denied_write_permission_preserves_the_previous_index() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = index_path(dir.path());
        let previous = RunRecord {
            id: "previous".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        write_run_index(&path, std::slice::from_ref(&previous)).unwrap();
        let original = fs::read(&path).unwrap();

        let mut denied = fs::metadata(dir.path()).unwrap().permissions();
        denied.set_mode(0o500);
        fs::set_permissions(dir.path(), denied).unwrap();

        let next = RunRecord {
            id: "next".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            ..Default::default()
        };
        let result = write_run_index(&path, std::slice::from_ref(&next));

        // Restore before any assertion can panic, so the tempdir's own cleanup on drop can still
        // remove it.
        let mut restored = fs::metadata(dir.path()).unwrap().permissions();
        restored.set_mode(0o700);
        fs::set_permissions(dir.path(), restored).unwrap();

        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}
