//! GitHub's `gh` integration. The boundary is deliberately small: GitHub JSON is parsed at the
//! driver edge, pure normalization and policy functions are injectable, and every shell/API
//! failure becomes a
//! degraded value instead of a panic. `GithubDriver` keeps caches per repository
//! instance, so two projects cannot leak one another's issues, comments, or reference status.

mod driver;
mod graphql;
mod model;
mod normalize;
mod remote;

pub use driver::{
    CommandOutput, CommandRunner, GithubDriver, SystemCommandRunner, build_pr_body,
    merge_preflight_allowed, normalize_merge_state,
};
pub use graphql::{
    GraphqlVariables, commit_checks_query, fetch_commit_checks, fetch_pr_checks,
    fetch_ref_statuses, pr_checks_query, ref_status_query,
};
pub use model::*;
pub use normalize::{
    CACHE_MS, COMMENT_BODY_CAP, COMMIT_CHECKS_CHUNK, CheckRun, CountsPage, GH_CHECKS_MAX,
    GH_COUNTS_MAX_PAGES, GH_MAX_LIMIT, GH_PR_DIFF_FILE_CAP, GH_PR_DIFF_JSON_CAP, GH_PR_PATCH_CAP,
    GH_REF_STATUS_MAX, REF_STATUS_CLOSED_TTL, REF_STATUS_MERGED_TTL, REF_STATUS_RETRY_MS,
    THREAD_ENTRY_CAP, TIMELINE_BUDGET_MS, TIMELINE_EVENT_CAP, TIMELINE_EVENT_KINDS,
    TIMELINE_MAX_PAGES, TIMELINE_MIN_PAGE_MS, TimelinePages, batch_recheck_after,
    derive_issue_reference_status, derive_pr_reference_status, fetch_comment_counts,
    fetch_pr_file_pages, fetch_timeline_pages, first_line, has_resolved_repository, merge_thread,
    normalize_comments, normalize_events, normalize_reviews, parse_counts_page, parse_owner_name,
    ref_number_from_url, ref_status_recheck_after, ref_status_ttl, rollup_to_checks,
    sanitize_ref_numbers,
};
pub use remote::{ParsedRemote, forge_kind_of_remote, forge_web_root, parse_remote, resolve_forge};
