use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The checks glyph enum from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChecksGlyph {
    Passing,
    Failing,
    Pending,
}

/// Mirrors `packages/contract/src/github.ts::GithubItem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubItem {
    pub kind: GithubItemKind,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub created_at: String,
    pub labels: Vec<String>,
    pub body: String,
    pub url: String,
    pub comments: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Option<ChecksGlyph>>,
}

/// The issue/PR kind discriminator from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubItemKind {
    Issue,
    Pr,
}

/// Mirrors `packages/contract/src/github.ts::GithubData`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubData {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<String>,
    pub issues: Vec<GithubItem>,
    pub prs: Vec<GithubItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_colors: Option<BTreeMap<String, String>>,
}

/// Mirrors the available branch of `GithubChecksData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubChecksAvailable {
    pub available: bool,
    pub checks: BTreeMap<String, Option<ChecksGlyph>>,
}

/// Mirrors the unavailable branch of `GithubChecksData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubChecksUnavailable {
    pub available: bool,
    pub reason: String,
}

/// Mirrors `packages/contract/src/github.ts::GithubChecksData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GithubChecksData {
    Available(GithubChecksAvailable),
    Unavailable(GithubChecksUnavailable),
}

/// Mirrors `packages/contract/src/github.ts::ReferenceStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceStatus {
    Draft,
    ReviewRequired,
    ChangesRequested,
    ChecksPending,
    ChecksFailing,
    Ready,
    Merged,
    Closed,
    Open,
    Completed,
    NotPlanned,
}

/// The maximum references in one status request.
pub const REFERENCE_STATUS_MAX: usize = 100;

/// Mirrors the available branch of `GithubRefStatusData`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubRefStatusAvailable {
    pub available: bool,
    pub prs: BTreeMap<String, ReferenceStatus>,
    pub issues: BTreeMap<String, ReferenceStatus>,
    pub recheck_after_ms: Option<f64>,
}

/// Mirrors the unavailable branch of `GithubRefStatusData`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubRefStatusUnavailable {
    pub available: bool,
    pub reason: String,
    pub recheck_after_ms: Option<f64>,
}

/// Mirrors `packages/contract/src/github.ts::GithubRefStatusData`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GithubRefStatusData {
    Available(GithubRefStatusAvailable),
    Unavailable(GithubRefStatusUnavailable),
}

/// Mirrors `packages/contract/src/github.ts::GithubComment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubComment {
    pub id: u64,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub created_at: String,
    pub body: String,
    pub kind: GithubCommentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<GithubReviewState>,
    pub url: String,
}

/// The comment/review discriminator from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubCommentKind {
    Comment,
    Review,
}

/// The review state discriminator from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

/// Mirrors `packages/contract/src/github.ts::GithubTimelineEventKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubTimelineEventKind {
    Committed,
    Labeled,
    Unlabeled,
    Assigned,
    Unassigned,
    Merged,
    Closed,
    Reopened,
    #[serde(rename = "head_ref_force_pushed")]
    HeadRefForcePushed,
    CrossReferenced,
    Renamed,
}

/// The optional label attached to a timeline event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubTimelineLabel {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Mirrors `packages/contract/src/github.ts::GithubTimelineEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTimelineEvent {
    pub id: String,
    pub kind: GithubTimelineEventKind,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Option<ChecksGlyph>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<GithubTimelineLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_is_pr: Option<bool>,
}

/// Mirrors `packages/contract/src/github.ts::GithubCommentsData`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubCommentsData {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub comments: Vec<GithubComment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<GithubTimelineEvent>>,
}

/// Mirrors `packages/contract/src/github.ts::GithubMergeMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubMergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// Mirrors `packages/contract/src/github.ts::GithubPrCheck`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPrCheck {
    pub name: String,
    pub state: GithubCheckState,
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The check state enum from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubCheckState {
    Passing,
    Failing,
    Pending,
    Unknown,
}

/// Mirrors `packages/contract/src/github.ts::GithubPrMergeState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPrMergeState {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: GithubPrState,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    pub head_sha: String,
    pub mergeable: GithubMergeable,
    pub review_decision: GithubReviewDecision,
    pub checks: Vec<GithubPrCheck>,
    pub methods: Vec<GithubMergeMethod>,
    pub default_method: Option<GithubMergeMethod>,
    pub eligibility: GithubMergeEligibility,
    pub blockers: Vec<GithubBlocker>,
    pub can_merge: bool,
    pub can_override: bool,
}

/// PR lifecycle state from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubPrState {
    Open,
    Closed,
    Merged,
}

/// Mergeability state from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubMergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// Review decision from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

/// Merge eligibility from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubMergeEligibility {
    Ready,
    Blocked,
    Pending,
    Unauthorized,
    Terminal,
    Unknown,
}

/// A merge blocker from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubBlocker {
    pub code: String,
    pub message: String,
}

/// Mirrors `packages/contract/src/github.ts::GithubPrMergeStateResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GithubPrMergeStateResponse {
    Available {
        available: bool,
        #[serde(rename = "mergeState")]
        merge_state: GithubPrMergeState,
    },
    Unavailable {
        available: bool,
        reason: String,
    },
}

/// Mirrors `packages/contract/src/github.ts::GithubMergeResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubMergeResponse {
    pub merged: bool,
    pub number: u64,
    pub url: String,
    pub method: GithubMergeMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit_sha: Option<String>,
}

/// Mirrors `packages/contract/src/github.ts::GithubPrChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPrChange {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub status: GithubChangeStatus,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_unavailable_reason: Option<GithubPatchUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// Changed-file status from `packages/contract/src/github.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubChangeStatus {
    Added,
    Modified,
    Removed,
    Renamed,
    Copied,
    Changed,
}

/// The reason a PR patch is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubPatchUnavailableReason {
    Binary,
    TooLarge,
    NotProvided,
}

/// Mirrors the available branch of `GithubPrChangesData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPrChangesAvailable {
    pub available: bool,
    pub number: u64,
    pub head_sha: String,
    pub files: Vec<GithubPrChange>,
    pub additions: u64,
    pub deletions: u64,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Mirrors the unavailable branch of `GithubPrChangesData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPrChangesUnavailable {
    pub available: bool,
    pub reason: String,
}

/// Mirrors `packages/contract/src/github.ts::GithubPrChangesData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GithubPrChangesData {
    Available(GithubPrChangesAvailable),
    Unavailable(GithubPrChangesUnavailable),
}
