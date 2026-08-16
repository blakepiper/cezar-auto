//! Pure plan checkpointing for intelligent context refresh.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanCheckpoint {
    pub completed_count: Option<usize>,
    pub last_snapshot_key: Option<String>,
    pub refreshes: u32,
}

pub const MAX_CONTEXT_REFRESHES: u32 = 32;

pub fn completed_count(entries: &[PlanEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.status == PlanStatus::Completed)
        .count()
}

pub fn next_entry(entries: &[PlanEntry]) -> Option<&PlanEntry> {
    entries
        .iter()
        .find(|entry| entry.status == PlanStatus::InProgress)
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.status == PlanStatus::Pending)
        })
}

pub fn snapshot_key(entries: &[PlanEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            let status = match entry.status {
                PlanStatus::Pending => "pending",
                PlanStatus::InProgress => "in-progress",
                PlanStatus::Completed => "completed",
                PlanStatus::Cancelled => "cancelled",
            };
            format!("{status}:{}", entry.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn refresh_prompt(entries: &[PlanEntry]) -> String {
    let next = next_entry(entries);
    let snapshot = entries
        .iter()
        .take(24)
        .map(|entry| {
            let marker = match entry.status {
                PlanStatus::Completed => "[x]",
                PlanStatus::InProgress => "[>]",
                PlanStatus::Cancelled => "[-]",
                PlanStatus::Pending => "[ ]",
            };
            format!(
                "{marker} {}",
                entry.content.chars().take(500).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Intelligent context refresh: the previous session completed a plan item and this task is continuing in a fresh context window.\n\nRead the handoff file and inspect the current worktree before acting. Do not restart, undo, or repeat completed work.\n\n{}\n\nCurrent plan snapshot:\n{}",
        next.map(|entry| format!("Focus next on: {}", entry.content))
            .unwrap_or_else(|| "Continue with the next unfinished plan item.".to_owned()),
        if snapshot.is_empty() {
            "(empty)"
        } else {
            &snapshot
        }
    )
}

/// Update one checkpoint and return a prompt only when a new completed item warrants a refresh.
pub fn observe_plan(
    checkpoint: &mut PlanCheckpoint,
    entries: &[PlanEntry],
    enabled: bool,
) -> Option<String> {
    let completed = completed_count(entries);
    let key = snapshot_key(entries);
    let previous = checkpoint.completed_count.replace(completed);
    let should_refresh = enabled
        && previous.is_some_and(|previous| completed > previous)
        && next_entry(entries).is_some()
        && checkpoint.refreshes < MAX_CONTEXT_REFRESHES
        && checkpoint.last_snapshot_key.as_deref() != Some(key.as_str());
    if should_refresh {
        checkpoint.last_snapshot_key = Some(key);
        checkpoint.refreshes += 1;
        Some(refresh_prompt(entries))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(statuses: &[(PlanStatus, &str)]) -> Vec<PlanEntry> {
        statuses
            .iter()
            .map(|(status, content)| PlanEntry {
                status: *status,
                content: (*content).to_owned(),
            })
            .collect()
    }

    #[test]
    fn first_snapshot_sets_a_baseline_and_later_completion_refreshes_once() {
        let mut checkpoint = PlanCheckpoint::default();
        assert!(
            observe_plan(
                &mut checkpoint,
                &entries(&[
                    (PlanStatus::InProgress, "one"),
                    (PlanStatus::Pending, "two")
                ]),
                true
            )
            .is_none()
        );
        assert!(
            observe_plan(
                &mut checkpoint,
                &entries(&[(PlanStatus::Completed, "one"), (PlanStatus::Pending, "two")]),
                true
            )
            .is_some()
        );
        assert!(
            observe_plan(
                &mut checkpoint,
                &entries(&[(PlanStatus::Completed, "one"), (PlanStatus::Pending, "two")]),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn refresh_cap_and_disabled_mode_are_deterministic() {
        let mut checkpoint = PlanCheckpoint {
            completed_count: Some(0),
            refreshes: MAX_CONTEXT_REFRESHES,
            ..PlanCheckpoint::default()
        };
        assert!(
            observe_plan(
                &mut checkpoint,
                &entries(&[(PlanStatus::Completed, "one"), (PlanStatus::Pending, "two")]),
                true
            )
            .is_none()
        );
        assert!(
            observe_plan(
                &mut checkpoint,
                &entries(&[(PlanStatus::Completed, "one"), (PlanStatus::Pending, "two")]),
                false
            )
            .is_none()
        );
    }
}
