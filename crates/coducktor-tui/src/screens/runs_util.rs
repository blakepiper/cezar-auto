//! Pure helpers shared by the Tasks and Global tasks screens.
//!
//! Shared task-table, grouping, attention, and formatting helpers. No rendering here.

use coducktor_contract::{ApiRun, DiffStat, ProcessUsage, RunStatus};

/// The statuses whose usage sample is current — a session is registered while
/// running AND while parked at `waiting` (the CLI process stays alive).
const USAGE_LIVE_STATUSES: [RunStatus; 3] =
    [RunStatus::Running, RunStatus::Idle, RunStatus::Waiting];

/// Finished outcomes, not gates — a `review` run still wants a human.
const FINISHED_STATUSES: [RunStatus; 3] =
    [RunStatus::Done, RunStatus::Failed, RunStatus::Cancelled];

/// The run a surface calls it — `titleSummary` is the display title when a
/// persisted record contains one, except for malformed auto/legacy summaries whose
/// sentence punctuation was persisted without following whitespace (#623).
pub fn run_title(run: &ApiRun) -> String {
    let record = &run.record;
    let Some(summary) = record.title_summary.as_deref() else {
        return record.title.clone();
    };
    let protected_title = matches!(
        record.title_origin,
        Some(coducktor_contract::TitleOrigin::User) | Some(coducktor_contract::TitleOrigin::Marker)
    );
    let malformed = summary
        .chars()
        .zip(summary.chars().skip(1))
        .any(|(a, b)| (a == '.' || a == '!' || a == '?') && b.is_ascii_uppercase());
    if protected_title || malformed {
        record.title.clone()
    } else {
        summary.to_owned()
    }
}

/// The status pill's attention grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attention {
    pub label: &'static str,
    pub tone: AttentionTone,
    pub pulse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionTone {
    Success,
    Pending,
    Danger,
    Violet,
    Neutral,
}

impl AttentionTone {
    pub fn style(self, theme: &crate::theme::Theme) -> ratatui::style::Style {
        let color = match self {
            Self::Success => theme.palette.done,
            Self::Pending => theme.palette.waiting,
            Self::Danger => theme.palette.failed,
            Self::Violet => theme.palette.review,
            Self::Neutral => theme.palette.soft_fg,
        };
        ratatui::style::Style::default().fg(color)
    }
}

/// `deriveAttention` — the canonical status → phrase/tone/pulse mapping.
pub fn attention(run: &ApiRun) -> Attention {
    let record = &run.record;
    if record.status == RunStatus::Failed && record.auto_resume_at.is_some() {
        return Attention {
            label: "scheduled",
            tone: AttentionTone::Pending,
            pulse: false,
        };
    }
    match record.status {
        RunStatus::Failed => Attention {
            label: "failed",
            tone: AttentionTone::Danger,
            pulse: false,
        },
        RunStatus::Waiting => Attention {
            label: "needs you",
            tone: AttentionTone::Pending,
            pulse: true,
        },
        RunStatus::Review => Attention {
            label: "needs review",
            tone: AttentionTone::Violet,
            pulse: true,
        },
        RunStatus::Running => Attention {
            label: "running",
            tone: AttentionTone::Violet,
            pulse: true,
        },
        RunStatus::Idle => Attention {
            label: "idle",
            tone: AttentionTone::Neutral,
            pulse: false,
        },
        RunStatus::Queued => Attention {
            label: "queued",
            tone: AttentionTone::Neutral,
            pulse: false,
        },
        RunStatus::Done => Attention {
            label: "done",
            tone: AttentionTone::Success,
            pulse: false,
        },
        RunStatus::Cancelled => Attention {
            label: "cancelled",
            tone: AttentionTone::Neutral,
            pulse: false,
        },
    }
}

/// An unread receipt: finished, not archived, not merely scheduled to resume.
pub fn is_unread(run: &ApiRun) -> bool {
    run.record.seen_at.is_none() && can_be_unread(run)
}

/// The set of runs that can actually carry an unread receipt.
pub fn can_be_unread(run: &ApiRun) -> bool {
    let record = &run.record;
    if record.archived || record.seen_at.is_some() {
        return false;
    }
    matches!(
        record.status,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
    )
}

/// A finished run that is read — dimmed in the row.
pub fn is_read_done_item(run: &ApiRun) -> bool {
    !run.record.archived
        && run.record.seen_at.is_some()
        && matches!(
            run.record.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        )
}

/// `$0.31` / `$12` — two decimals until the cents stop mattering; `''` when the
/// run has no recorded spend.
pub fn format_cost(usd: Option<f64>) -> String {
    match usd {
        Some(usd) if usd > 0.0 => {
            if usd >= 10.0 {
                format!("${usd:.0}")
            } else {
                format!("${usd:.2}")
            }
        }
        _ => String::new(),
    }
}

/// [`format_cost`] plus a compact `×N` marker when the run split its cost across more than one
/// model — same-provider failover or a mid-run model change, the case a single blended total
/// hides. `model_usage` is only ever `Some` for more than one distinct model, so its presence
/// alone is the signal; the marker doesn't need to re-derive that count itself.
pub fn format_cost_with_split(
    usd: Option<f64>,
    model_usage: Option<&[coducktor_contract::ModelUsageEntry]>,
) -> String {
    let cost = format_cost(usd);
    match model_usage {
        Some(usage) if !usage.is_empty() => format!("{cost} ×{}", usage.len()),
        _ => cost,
    }
}

/// Format memory usage as `612 MB` or `1.2 GB`.
pub fn format_mem(bytes: Option<f64>) -> String {
    match bytes {
        Some(bytes) if bytes > 0.0 => {
            let gib = 1024.0_f64.powi(3);
            let mib = 1024.0_f64.powi(2);
            if bytes >= gib {
                format!("{:.1} GB", bytes / gib)
            } else if bytes >= mib {
                format!("{} MB", (bytes / mib).round())
            } else {
                format!("{} kB", (bytes / 1024.0).round())
            }
        }
        _ => String::new(),
    }
}

/// `1.2k` / `96.2k` / `1.4M` — truncates rather than rounds.
pub fn compact_tokens(tokens: f64) -> String {
    if !tokens.is_finite() || tokens <= 0.0 {
        return "0".to_owned();
    }
    if tokens >= 1_000_000.0 {
        format!("{:.1}M", (tokens / 100_000.0).floor() / 10.0)
    } else if tokens >= 1_000.0 {
        format!("{:.1}k", (tokens / 100.0).floor() / 10.0)
    } else {
        format!("{}", tokens.floor())
    }
}

/// The tokens cell: `1.2k / 300` — the table variant of `DirectionalUsage`.
pub fn format_tokens(input: Option<f64>, output: Option<f64>) -> String {
    match (input, output) {
        (Some(input), Some(output)) => {
            format!("{} / {}", compact_tokens(input), compact_tokens(output))
        }
        (Some(input), None) => compact_tokens(input),
        (None, Some(output)) => format!("OUT {}", compact_tokens(output)),
        (None, None) => String::new(),
    }
}

/// The ± cell: `+12 −3`.
pub fn format_diff(stat: Option<&DiffStat>) -> String {
    match stat {
        Some(stat) => format!("+{} −{}", stat.adds, stat.dels),
        None => String::new(),
    }
}

/// The workflow cell's text: a `(planned)` chain reads as its first
/// agent step's name.
pub fn workflow_label(run: &ApiRun) -> String {
    if run.record.workflow == "(planned)"
        && let Some(step) = run
            .record
            .steps
            .iter()
            .find(|step| step.kind == coducktor_contract::StepKind::Agent)
        && !step.name.is_empty()
    {
        return step.name.clone();
    }
    run.record.workflow.clone()
}

/// One CPU or Mem cell: the text plus whether it is a live sample or a peak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCell {
    pub text: String,
    pub kind: UsageKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKind {
    Live,
    Peak,
    None,
}

/// The CPU/Mem cells: a live sample is only believed while the run's process
/// tree can exist; a finished run falls back to its persisted peak, dimmed.
pub fn usage_cells(run: &ApiRun, sample: Option<&ProcessUsage>) -> (UsageCell, UsageCell) {
    let live = if USAGE_LIVE_STATUSES.contains(&run.record.status) {
        sample
    } else {
        None
    };
    if let Some(live) = live {
        return (
            UsageCell {
                text: format!("{}%", live.cpu_pct.round()),
                kind: UsageKind::Live,
            },
            UsageCell {
                text: format_mem(Some(live.rss_bytes)),
                kind: UsageKind::Live,
            },
        );
    }
    let peak_mem = format_mem(run.record.peak_rss_bytes);
    let mem = if peak_mem.is_empty() {
        UsageCell {
            text: String::new(),
            kind: UsageKind::None,
        }
    } else {
        UsageCell {
            text: format!("peak {peak_mem}"),
            kind: UsageKind::Peak,
        }
    };
    (
        UsageCell {
            text: String::new(),
            kind: UsageKind::None,
        },
        mem,
    )
}

/// The strongest tracker reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReference {
    pub kind: &'static str,
    pub number: String,
    pub url: Option<String>,
}

pub fn task_reference(run: &ApiRun) -> Option<TaskReference> {
    let record = &run.record;
    let mut pr_url = record.pull_request_url.clone();
    let suppress_about_pr = record
        .marker_refs
        .as_ref()
        .is_some_and(|refs| refs.issue.is_some() && refs.pr.is_none());
    if !suppress_about_pr
        && pr_url.is_none()
        && let Some(about) = &record.referenced_pull_request_url
    {
        pr_url = Some(about.clone());
    }
    if let Some(url) = pr_url {
        return Some(TaskReference {
            kind: "PR",
            number: pr_number(&url),
            url: Some(url),
        });
    }
    if let Some(url) = &record.referenced_issue_url {
        return Some(TaskReference {
            kind: "issue",
            number: pr_number(url),
            url: Some(url.clone()),
        });
    }
    if let Some(number) = record.issue_number {
        return Some(TaskReference {
            kind: "issue",
            number: format!("{number:.0}"),
            url: None,
        });
    }
    if let Some(number) = record.pr_number {
        return Some(TaskReference {
            kind: "PR",
            number: format!("{number:.0}"),
            url: None,
        });
    }
    None
}

fn pr_number(url: &str) -> String {
    url.split('/')
        .next_back()
        .filter(|last| !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(url)
        .to_owned()
}

/// One row of a variant-comparison strip below the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareGroup {
    pub group_id: String,
    pub title: String,
    pub count: usize,
}

/// Variant groups whose members are all terminal — the ones a Compare link can
/// honestly offer.
pub fn compare_groups(runs: &[ApiRun], view: TaskView) -> Vec<CompareGroup> {
    let in_view: Vec<&ApiRun> = runs
        .iter()
        .filter(|run| run.record.archived == (view == TaskView::Archived))
        .collect();
    let mut by_group: std::collections::BTreeMap<&str, Vec<&ApiRun>> = Default::default();
    for run in in_view {
        if let Some(group_id) = run.record.group_id.as_deref() {
            by_group.entry(group_id).or_default().push(run);
        }
    }
    by_group
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .filter(|(_, members)| {
            members.iter().all(|run| {
                matches!(
                    run.record.status,
                    RunStatus::Done | RunStatus::Failed | RunStatus::Review | RunStatus::Cancelled
                )
            })
        })
        .map(|(group_id, members)| {
            let title = group_title(members[0]);
            CompareGroup {
                group_id: group_id.to_owned(),
                title,
                count: members.len(),
            }
        })
        .collect()
}

/// A variant's shared title: `"Add autocomplete (A)"` → `"Add autocomplete"`.
pub fn group_title(run: &ApiRun) -> String {
    let mut title = run.record.title.clone();
    for variant in ["(A)", "(B)", "(C)"] {
        if title.ends_with(variant) {
            title.truncate(title.len() - variant.len());
            break;
        }
    }
    title
}

/// The Active/Archived split shared by the Tasks screen and the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskView {
    Active,
    Archived,
}

/// How many active runs have finished.
pub fn finished_run_count(runs: &[ApiRun]) -> usize {
    runs.iter()
        .filter(|run| !run.record.archived && FINISHED_STATUSES.contains(&run.record.status))
        .count()
}

/// Card search covers the exact prompt plus every compact reference field shown on a card.
pub fn filter_runs<'a>(runs: &'a [ApiRun], query: &str) -> Vec<&'a ApiRun> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return runs.iter().collect();
    }
    runs.iter()
        .filter(|run| {
            [
                run_title(run).to_ascii_lowercase(),
                run.record.task.to_ascii_lowercase(),
                run.record
                    .branch
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                run.record.workflow.to_ascii_lowercase(),
                workflow_label(run).to_ascii_lowercase(),
                run.record
                    .pull_request_url
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                run.record
                    .referenced_pull_request_url
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                run.record
                    .referenced_issue_url
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            ]
            .iter()
            .any(|text| text.contains(&needle))
        })
        .collect()
}

/// Default ordering: needs-you first,
/// then the pipeline, then outcomes; ties break on recency.
pub fn sort_runs(runs: &[ApiRun], view: TaskView) -> Vec<usize> {
    let mut indexed: Vec<usize> = runs
        .iter()
        .enumerate()
        .filter(|(_, run)| run.record.archived == (view == TaskView::Archived))
        .map(|(index, _)| index)
        .collect();
    indexed.sort_by(|&a, &b| {
        let a_run = &runs[a];
        let b_run = &runs[b];
        let weight_a = status_weight(a_run);
        let weight_b = status_weight(b_run);
        let weight = weight_a.cmp(&weight_b);
        if weight != std::cmp::Ordering::Equal {
            return weight;
        }
        if weight_a == 3
            && let (Some(a_at), Some(b_at)) = (
                a_run.record.auto_resume_at.as_deref(),
                b_run.record.auto_resume_at.as_deref(),
            )
        {
            let order = a_at.cmp(b_at);
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        if a_run.record.status == RunStatus::Queued && b_run.record.status == RunStatus::Queued {
            let order = a_run.record.created_at.cmp(&b_run.record.created_at);
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        b_run.record.created_at.cmp(&a_run.record.created_at)
    });
    indexed
}

fn status_weight(run: &ApiRun) -> u8 {
    if run.record.status == RunStatus::Failed && run.record.auto_resume_at.is_some() {
        return 3;
    }
    match run.record.status {
        RunStatus::Waiting => 0,
        RunStatus::Review => 1,
        RunStatus::Running => 2,
        RunStatus::Idle => 3,
        RunStatus::Queued => 4,
        RunStatus::Done => 5,
        RunStatus::Failed => 6,
        RunStatus::Cancelled => 7,
    }
}

/// Queue positions over the active queued runs, in creation order (the order
/// the engine actually starts them).
pub fn queue_positions(runs: &[ApiRun]) -> std::collections::HashMap<String, usize> {
    let mut queued: Vec<&ApiRun> = runs
        .iter()
        .filter(|run| !run.record.archived && run.record.status == RunStatus::Queued)
        .collect();
    queued.sort_by(|a, b| a.record.created_at.cmp(&b.record.created_at));
    queued
        .iter()
        .enumerate()
        .map(|(index, run)| (run.record.id.clone(), index + 1))
        .collect()
}

/// `3s`, `12m`, `4h`, `2d`, with negative elapsed time clamped to zero.
pub fn short_age(iso: &str, now_epoch_secs: i64) -> String {
    let Some(then) = parse_iso_seconds(iso) else {
        return String::new();
    };
    let seconds = (now_epoch_secs - then).max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// The UTC clock time of an ISO stamp as `HH:MM` — the scheduled-resume pill.
pub fn clock_time(iso: &str) -> Option<String> {
    let epoch = parse_iso_seconds(iso)?;
    let hour = (epoch % 86_400) / 3600;
    let minute = (epoch % 3600) / 60;
    Some(format!("{hour:02}:{minute:02}"))
}

/// A lenient ISO-8601 instant parser good enough for coducktor's UTC timestamps.
/// Handles `YYYY-MM-DDTHH:MM:SS(.sss)(Z|±HH:MM)`, returns epoch seconds.
pub fn parse_iso_seconds(iso: &str) -> Option<i64> {
    let (date_part, rest) = iso.split_once('T')?;
    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    let (time_part, offset) = split_offset(rest)?;
    let mut time = time_part.split(':');
    let hour: u32 = time.next()?.parse().ok()?;
    let minute: u32 = time.next()?.parse().ok()?;
    let second: f64 = time.next()?.parse().ok()?;
    let mut epoch = days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + second.floor() as i64;
    epoch -= offset_seconds(offset);
    Some(epoch)
}

fn split_offset(time: &str) -> Option<(&str, &str)> {
    if let Some(rest) = time.strip_suffix('Z') {
        return Some((rest, "Z"));
    }
    let split = time
        .char_indices()
        .find(|(_, c)| *c == '+' || *c == '-')
        .map(|(index, _)| index);
    if let Some(index) = split {
        Some((&time[..index], &time[index..]))
    } else {
        Some((time, "Z"))
    }
}

fn offset_seconds(offset: &str) -> i64 {
    if offset == "Z" {
        return 0;
    }
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let digits = offset.trim_start_matches(['+', '-']);
    let (hour, minute) = match digits.split_once(':') {
        Some((hour, minute)) => (hour, minute),
        None => {
            if digits.len() == 4 {
                (&digits[..2], &digits[2..])
            } else {
                return 0;
            }
        }
    };
    let hour: i64 = hour.parse().unwrap_or(0);
    let minute: i64 = minute.parse().unwrap_or(0);
    sign * (hour * 3600 + minute * 60)
}

/// Days from 1970-01-01 (Howard Hinnant's algorithm, public domain).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * i64::from(if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
        + i64::from(day)
        - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use coducktor_contract::{ApiRun, RunRecord, RunStatus};

    use super::*;

    fn run(id: &str, status: RunStatus, created_at: &str) -> ApiRun {
        ApiRun {
            record: RunRecord {
                id: id.to_owned(),
                title: format!("Task {id}"),
                workflow: "quick-task".to_owned(),
                task: String::new(),
                status,
                created_at: created_at.to_owned(),
                tokens_used: 0.0,
                archived: false,
                steps: Vec::new(),
                ..RunRecord::default()
            },
            usage: None,
        }
    }

    #[test]
    fn cost_with_split_flags_a_run_that_used_more_than_one_model() {
        assert_eq!(format_cost_with_split(Some(1.20), None), "$1.20");
        let usage = vec![
            coducktor_contract::ModelUsageEntry {
                model: "claude-sonnet".to_owned(),
                reasoning_effort: None,
                pct: 75.0,
            },
            coducktor_contract::ModelUsageEntry {
                model: "gpt-5.1-codex".to_owned(),
                reasoning_effort: None,
                pct: 25.0,
            },
        ];
        assert_eq!(format_cost_with_split(Some(1.20), Some(&usage)), "$1.20 ×2");
    }

    #[test]
    fn cost_and_memory_format_for_terminal_display() {
        assert_eq!(format_cost(Some(0.31)), "$0.31");
        assert_eq!(format_cost(Some(12.0)), "$12");
        assert_eq!(format_cost(None), "");
        assert_eq!(format_mem(Some(612.0 * 1024.0 * 1024.0)), "612 MB");
        assert_eq!(format_mem(Some(1.2 * 1024.0 * 1024.0 * 1024.0)), "1.2 GB");
        assert_eq!(format_mem(None), "");
        assert_eq!(compact_tokens(96_200.0), "96.2k");
        assert_eq!(compact_tokens(1_400_000.0), "1.4M");
    }

    #[test]
    fn default_order_puts_needs_you_first_and_queued_fifo() {
        let runs = vec![
            run("old-done", RunStatus::Done, "2026-08-15T00:00:00Z"),
            run("wait", RunStatus::Waiting, "2026-08-15T00:00:00Z"),
            run("queued-1", RunStatus::Queued, "2026-08-15T00:00:00Z"),
            run("queued-2", RunStatus::Queued, "2026-08-15T00:00:01Z"),
            run("new-done", RunStatus::Done, "2026-08-15T00:00:02Z"),
        ];
        let order = sort_runs(&runs, TaskView::Active);
        let ids: Vec<&str> = order.iter().map(|&i| runs[i].record.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["wait", "queued-1", "queued-2", "new-done", "old-done"]
        );
    }

    #[test]
    fn search_matches_title_branch_and_workflow() {
        let mut task = run("r1", RunStatus::Running, "2026-08-15T00:00:00Z");
        task.record.branch = Some("feat/search".to_owned());
        let runs = vec![task, run("r2", RunStatus::Running, "2026-08-15T00:00:00Z")];
        assert_eq!(filter_runs(&runs, "SEARCH").len(), 1);
        assert_eq!(filter_runs(&runs, "task r2").len(), 1);
        assert_eq!(filter_runs(&runs, "").len(), 2);
    }

    #[test]
    fn iso_timestamps_parse_and_age() {
        let epoch = parse_iso_seconds("2026-08-15T00:00:00Z").unwrap();
        assert!(epoch > 1_700_000_000);
        assert_eq!(short_age("2026-08-15T00:00:00Z", epoch + 86_400), "1d");
        assert_eq!(short_age("2026-08-15T00:01:00Z", epoch + 120), "1m");
        assert_eq!(short_age("not-a-date", 0), "");
    }

    #[test]
    fn attention_maps_statuses_for_terminal_display() {
        assert_eq!(
            attention(&run("w", RunStatus::Waiting, "now")).label,
            "needs you"
        );
        assert_eq!(
            attention(&run("r", RunStatus::Review, "now")).label,
            "needs review"
        );
        assert_eq!(attention(&run("d", RunStatus::Done, "now")).label, "done");
        assert_eq!(
            attention(&run("f", RunStatus::Failed, "now")).label,
            "failed"
        );

        let mut scheduled = run("s", RunStatus::Failed, "now");
        scheduled.record.auto_resume_at = Some("2026-08-15T01:00:00Z".to_owned());
        assert_eq!(attention(&scheduled).label, "scheduled");
    }

    #[test]
    fn compare_strip_only_offers_terminal_groups() {
        let mut a = run("a", RunStatus::Done, "2026-08-15T00:00:00Z");
        a.record.group_id = Some("g1".to_owned());
        a.record.title = "Compare me (A)".to_owned();
        let mut b = run("b", RunStatus::Review, "2026-08-15T00:00:00Z");
        b.record.group_id = Some("g1".to_owned());
        b.record.title = "Compare me (B)".to_owned();
        let mut c = run("c", RunStatus::Running, "2026-08-15T00:00:00Z");
        c.record.group_id = Some("g2".to_owned());
        let mut d = run("d", RunStatus::Running, "2026-08-15T00:00:00Z");
        d.record.group_id = Some("g2".to_owned());
        let runs = vec![a, b, c, d];
        let groups = compare_groups(&runs, TaskView::Active);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "g1");
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn finished_sweep_follows_status_rules() {
        let mut done = run("d", RunStatus::Done, "2026-08-15T00:00:00Z");
        done.record.seen_at = None;
        let mut read = run("r", RunStatus::Done, "2026-08-15T00:00:00Z");
        read.record.seen_at = Some("2026-08-15T01:00:00Z".to_owned());
        let mut waiting = run("w", RunStatus::Waiting, "2026-08-15T00:00:00Z");
        waiting.record.archived = true;
        let runs = vec![done.clone(), read.clone(), waiting];
        // Both active done runs are "finished" — read receipts don't un-finish a run.
        assert_eq!(finished_run_count(&runs), 2);
    }
}
