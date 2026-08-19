//! In-band task-reference markers.
//!
//! The main agent thread declares its subject PR/issue — and optionally a title — the same
//! way it declares completion with `DUCK:DONE`. Parsed from the accumulated turn text only
//! (the agent's own words, never tool output), so a task that merely *reads* the marker
//! contract cannot poison its record. Marker values outrank the fuzzy discovery layers.
//!
//! The legacy marker spelling parses identically through the compatibility regex.

use std::sync::LazyLock;

use regex::Regex;

use super::task_refs::MAX_REF;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TaskMarkers {
    pub pr: Option<i64>,
    pub issue: Option<i64>,
    pub title: Option<String>,
}

// Line-anchored so prose that mentions a marker never parses; the instruction fragment's
// own `DUCK:PR=<number>` placeholder is non-numeric and inert.
//
// This is the one marker-vocabulary compatibility regex. It
// canonicalizes both the current and legacy prefixes before the narrower parsers below run,
// so in-flight sessions and unrewritten skills keep working without letting old spelling leak
// into newly emitted prompts.
static MARKER_PREFIX_RE: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"(?P<prefix>CEZ|DUCK):(?P<kind>DONE|MONITORING|ASK|PR|ISSUE|TITLE)(?P<separator>=|[ \t]+)?",
    )
});

fn available_regex(pattern: &LazyLock<Result<Regex, regex::Error>>) -> Option<&Regex> {
    pattern.as_ref().ok()
}

/// Convert either marker prefix to the writer's current spelling. The replacement deliberately
/// keeps the payload untouched, including a multi-line ask JSON payload.
pub fn canonicalize_markers(text: &str) -> String {
    available_regex(&MARKER_PREFIX_RE).map_or_else(
        || text.to_owned(),
        |regex| {
            regex
                .replace_all(text, |caps: &regex::Captures<'_>| {
                    format!(
                        "DUCK:{}{}",
                        &caps["kind"],
                        caps.name("separator").map_or("", |match_| match_.as_str())
                    )
                })
                .into_owned()
        },
    )
}

static PR_MARKER_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^DUCK:PR=(\d+)\s*$"));
static ISSUE_MARKER_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^DUCK:ISSUE=(\d+)\s*$"));
static TITLE_MARKER_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^DUCK:TITLE=(.+)$"));

// Report-tier reference lines: the human-friendly chaining lines pipeline skills end their
// reports with — `PR: #12 (link: https://…/pull/12)`
// — plus the legacy env-style markers older skill versions printed. Same trust boundary as
// marker lines (parsed from the agent's own turn text only), one notch below in precedence: an
// explicit DUCK:PR / DUCK:ISSUE in the same turn wins.
static REPORT_PR_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^PR: #(\d+) \(link: \S+\)\s*$"));
static REPORT_ISSUE_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^Issue: #(\d+) \(link: \S+\)\s*$"));
static LEGACY_PR_NUMBER_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^PR_NUMBER=(\d+)\s*$"));
static LEGACY_PR_URL_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^PR_URL=\S*/pull/(\d+)\s*$"));
static LEGACY_ISSUE_NUMBER_RE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^ISSUE_NUMBER=(\d+)\s*$"));

fn last_number(text: &str, re: Option<&Regex>) -> Option<i64> {
    let re = re?;
    let mut value = None;
    for caps in re.captures_iter(text) {
        if let Ok(n) = caps[1].parse::<i64>()
            && n > 0
            && n < MAX_REF
        {
            value = Some(n);
        }
    }
    value
}

/// The turn's declared references. The last occurrence of each marker wins — an agent that
/// corrects itself mid-turn is believed, not averaged. Within a turn, an explicit DUCK:*
/// declaration outranks a report-tier line, which outranks the legacy env-style markers.
pub fn parse_task_markers(text: &str) -> TaskMarkers {
    let text = canonicalize_markers(text);
    let pr = last_number(&text, available_regex(&PR_MARKER_RE))
        .or_else(|| last_number(&text, available_regex(&REPORT_PR_RE)))
        .or_else(|| last_number(&text, available_regex(&LEGACY_PR_NUMBER_RE)))
        .or_else(|| last_number(&text, available_regex(&LEGACY_PR_URL_RE)));
    let issue = last_number(&text, available_regex(&ISSUE_MARKER_RE))
        .or_else(|| last_number(&text, available_regex(&REPORT_ISSUE_RE)))
        .or_else(|| last_number(&text, available_regex(&LEGACY_ISSUE_NUMBER_RE)));
    let mut title = None;
    if let Some(regex) = available_regex(&TITLE_MARKER_RE) {
        for caps in regex.captures_iter(&text) {
            let t = caps[1].trim();
            if !t.is_empty() {
                title = Some(t.to_owned());
            }
        }
    }
    TaskMarkers { pr, issue, title }
}

static MARKER_LINE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^DUCK:(?:PR=\d+|ISSUE=\d+|TITLE=.+)\s*$"));

/// Remove complete marker lines from display text — the `stripDoneMarker` precedent. Only
/// control lines (`DUCK:*` and the legacy spelling) are stripped: the report-tier
/// reference lines (`PR: #12 (link: …)`) are human-readable by design and stay visible.
pub fn strip_task_markers(text: &str) -> String {
    let text = canonicalize_markers(text);
    if !text.contains("DUCK:") {
        return text.to_owned();
    }
    text.split('\n')
        .filter(|line| !available_regex(&MARKER_LINE).is_some_and(|regex| regex.is_match(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(pr: Option<i64>, issue: Option<i64>, title: Option<&str>) -> TaskMarkers {
        TaskMarkers {
            pr,
            issue,
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn reads_each_marker_off_its_own_line() {
        assert_eq!(
            parse_task_markers(
                "Working on it.\nDUCK:PR=442\nDUCK:ISSUE=433\nDUCK:TITLE=fixing plan rendering\ndone soon"
            ),
            markers(Some(442), Some(433), Some("fixing plan rendering"))
        );
    }

    #[test]
    fn reads_and_strips_the_legacy_marker_spelling() {
        let legacy = concat!("C", "E", "Z");
        let text = format!("Working on it.\n{legacy}:PR=442\n{legacy}:TITLE=fixing plan rendering");
        assert_eq!(
            parse_task_markers(&text),
            markers(Some(442), None, Some("fixing plan rendering"))
        );
        assert_eq!(strip_task_markers(&text), "Working on it.");
    }

    #[test]
    fn the_last_occurrence_of_a_marker_wins() {
        assert_eq!(
            parse_task_markers("DUCK:PR=1\nsome progress\nDUCK:PR=500"),
            markers(Some(500), None, None)
        );
        assert_eq!(
            parse_task_markers("DUCK:TITLE=first guess\nDUCK:TITLE=implementing comment threads"),
            markers(None, None, Some("implementing comment threads"))
        );
    }

    #[test]
    fn is_line_anchored_prose_mentions_and_inline_text_never_parse() {
        assert_eq!(
            parse_task_markers("I will emit DUCK:PR=442 when the PR exists"),
            TaskMarkers::default()
        );
        assert_eq!(parse_task_markers("  DUCK:PR=442"), TaskMarkers::default());
        assert_eq!(
            parse_task_markers("DUCK:PR=442 (the review PR)"),
            TaskMarkers::default()
        );
    }

    #[test]
    fn the_instruction_placeholder_and_junk_values_are_inert() {
        assert_eq!(
            parse_task_markers("DUCK:PR=<number>"),
            TaskMarkers::default()
        );
        assert_eq!(parse_task_markers("DUCK:PR="), TaskMarkers::default());
        assert_eq!(parse_task_markers("DUCK:PR=0"), TaskMarkers::default());
        assert_eq!(
            parse_task_markers("DUCK:PR=99999999999"),
            TaskMarkers::default()
        );
        assert_eq!(parse_task_markers("DUCK:TITLE=   "), TaskMarkers::default());
    }

    #[test]
    fn tolerates_trailing_whitespace_and_crlf_line_endings() {
        assert_eq!(
            parse_task_markers("DUCK:PR=7  \r\nDUCK:ISSUE=9\r\n"),
            markers(Some(7), Some(9), None)
        );
    }

    #[test]
    fn finds_nothing_in_plain_prose() {
        assert_eq!(
            parse_task_markers("renamed the settings page"),
            TaskMarkers::default()
        );
        assert_eq!(parse_task_markers(""), TaskMarkers::default());
    }

    #[test]
    fn accepts_the_duck_spelling_coducktor_now_emits() {
        assert_eq!(
            parse_task_markers(
                "Working on it.\nDUCK:PR=442\nDUCK:ISSUE=433\nDUCK:TITLE=fixing plan rendering\ndone soon"
            ),
            markers(Some(442), Some(433), Some("fixing plan rendering"))
        );
        assert_eq!(
            parse_task_markers("DUCK:PR=1\nsome progress\nDUCK:PR=500"),
            markers(Some(500), None, None)
        );
    }

    #[test]
    fn reads_the_human_friendly_pr_issue_report_lines() {
        let report = [
            "om-auto-create-pr: add dark mode",
            "Issue: #433 (link: https://github.com/open-mercato/coducktor/issues/433)",
            "PR: #442 (link: https://github.com/open-mercato/coducktor/pull/442)",
            "Status: complete",
        ]
        .join("\n");
        assert_eq!(
            parse_task_markers(&report),
            markers(Some(442), Some(433), None)
        );
    }

    #[test]
    fn a_legacy_declaration_in_the_same_turn_outranks_a_report_line() {
        assert_eq!(
            parse_task_markers("DUCK:PR=7\nPR: #442 (link: https://github.com/o/r/pull/442)"),
            markers(Some(7), None, None)
        );
        assert_eq!(
            parse_task_markers("Issue: #9 (link: https://github.com/o/r/issues/9)\nDUCK:ISSUE=3"),
            markers(None, Some(3), None)
        );
    }

    #[test]
    fn still_accepts_the_legacy_env_style_markers_from_older_skill_versions() {
        assert_eq!(
            parse_task_markers("PR_URL=https://github.com/o/r/pull/442\nPR_NUMBER=442"),
            markers(Some(442), None, None)
        );
        assert_eq!(
            parse_task_markers("PR_URL=https://github.com/o/r/pull/442"),
            markers(Some(442), None, None)
        );
        assert_eq!(
            parse_task_markers("ISSUE_NUMBER=12"),
            markers(None, Some(12), None)
        );
    }

    #[test]
    fn a_report_line_outranks_a_legacy_marker_the_last_report_line_wins() {
        assert_eq!(
            parse_task_markers("PR_NUMBER=1\nPR: #2 (link: https://github.com/o/r/pull/2)"),
            markers(Some(2), None, None)
        );
        assert_eq!(
            parse_task_markers(
                "PR: #1 (link: https://github.com/o/r/pull/1)\nPR: #2 (link: https://github.com/o/r/pull/2)"
            ),
            markers(Some(2), None, None)
        );
    }

    #[test]
    fn is_line_anchored_and_exact_shape_for_report_lines() {
        assert_eq!(
            parse_task_markers(
                "the report ends with PR: #442 (link: https://github.com/o/r/pull/442)"
            ),
            TaskMarkers::default()
        );
        assert_eq!(
            parse_task_markers("PR: #<PR number> (link: <full PR URL>)"),
            TaskMarkers::default()
        );
        assert_eq!(
            parse_task_markers("- PR: #442 (link: https://github.com/o/r/pull/442)"),
            TaskMarkers::default()
        );
        assert_eq!(
            parse_task_markers("PR: #442 (link: https://github.com/o/r/pull/442) — merged"),
            TaskMarkers::default()
        );
        assert_eq!(parse_task_markers("PR: 442"), TaskMarkers::default());
    }

    #[test]
    fn report_lines_tolerate_trailing_whitespace_and_crlf() {
        assert_eq!(
            parse_task_markers("PR: #7 (link: https://github.com/o/r/pull/7)  \r\n"),
            markers(Some(7), None, None)
        );
    }

    #[test]
    fn strip_removes_complete_marker_lines_and_keeps_the_surrounding_text() {
        assert_eq!(
            strip_task_markers(
                "Opened the PR.\nDUCK:PR=442\nDUCK:TITLE=fixing plan rendering\nNext: tests."
            ),
            "Opened the PR.\nNext: tests."
        );
    }

    #[test]
    fn strip_leaves_prose_mentions_and_non_marker_lines_alone() {
        let text = "I will emit DUCK:PR=442 later\nnormal line";
        assert_eq!(strip_task_markers(text), text);
    }

    #[test]
    fn strip_is_a_noop_on_text_without_any_legacy_prefix() {
        assert_eq!(
            strip_task_markers("plain progress update"),
            "plain progress update"
        );
    }

    #[test]
    fn strip_leaves_report_tier_reference_lines_visible() {
        let text = "PR: #442 (link: https://github.com/o/r/pull/442)\nDUCK:PR=442\ndone";
        assert_eq!(
            strip_task_markers(text),
            "PR: #442 (link: https://github.com/o/r/pull/442)\ndone"
        );
    }

    #[test]
    fn strip_removes_duck_control_lines_too() {
        assert_eq!(
            strip_task_markers(
                "Opened the PR.\nDUCK:PR=442\nDUCK:TITLE=fixing plan rendering\nNext: tests."
            ),
            "Opened the PR.\nNext: tests."
        );
    }
}
