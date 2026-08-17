use std::path::PathBuf;

use coducktor_contract::{
    ChecksGlyph, GithubCheckState, GithubCommentsData, GithubData, GithubItem, GithubItemKind,
    GithubMergeMethod, GithubPrChangesData, GithubPrCheck, GithubPrMergeState, GithubPrState,
    ReferenceStatus, RunRecord,
};

/// The forge-driver seam used by the client and TUI integrations. GitHub is the first
/// implementation; keeping these values outside transport details makes a later GitLab driver a
/// local addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    Github,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeAvailability {
    pub available: bool,
    pub reason: Option<String>,
}

impl ForgeAvailability {
    pub fn available() -> Self {
        Self {
            available: true,
            reason: None,
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepoRef {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeRefKind {
    Repo,
    Issue,
    Pr,
    Branch,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePrStatus {
    pub number: u64,
    pub url: String,
    pub state: GithubPrState,
    pub is_draft: bool,
    pub checks: Option<ChecksGlyph>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgePrMergeStateResult {
    Available(GithubPrMergeState),
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeMergeInput {
    pub method: GithubMergeMethod,
    pub expected_head_sha: String,
    pub override_rules: bool,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeMergeResult {
    Merged {
        number: u64,
        url: String,
        method: GithubMergeMethod,
        merge_commit_sha: Option<String>,
    },
    Rejected {
        status: u16,
        error: String,
        code: Option<String>,
        current: Option<GithubPrMergeState>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftPrInput {
    pub repo_root: PathBuf,
    pub run: RunRecord,
    pub handoff_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftPrOutcome {
    Created { url: String, dry_run: bool },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReference {
    pub kind: ReferenceKind,
    pub status: ReferenceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Pr,
    Issue,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefStatusBatch {
    pub resolved: std::collections::BTreeMap<u64, ResolvedReference>,
    pub failed: Vec<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GithubRefStatusData {
    pub available: bool,
    pub reason: Option<String>,
    pub prs: std::collections::BTreeMap<u64, ReferenceStatus>,
    pub issues: std::collections::BTreeMap<u64, ReferenceStatus>,
    pub recheck_after_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPrChange {
    pub path: String,
    pub previous_path: Option<String>,
    pub status: coducktor_contract::GithubChangeStatus,
    pub additions: u64,
    pub deletions: u64,
    pub patch: Option<String>,
    pub patch_unavailable_reason: Option<coducktor_contract::GithubPatchUnavailableReason>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubPrDiffResult {
    Available {
        number: u64,
        head_sha: String,
        files: Vec<GithubPrChange>,
        additions: u64,
        deletions: u64,
        truncated: bool,
        reason: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

pub type ForgePrDiffResult = GithubPrDiffResult;

pub type GithubPrChanges = GithubPrChangesData;
pub type CheckState = GithubCheckState;
pub type PrCheck = GithubPrCheck;

/// Common forge operations consumed by the client. Methods return owned values because the
/// implementation may have filled them from a bounded cache or a subprocess response.
pub trait ForgeDriver: Send + Sync {
    fn kind(&self) -> ForgeKind;
    fn detect(&self) -> ForgeAvailability;
    fn detect_cached(&self) -> Option<ForgeAvailability>;
    fn list_issues(&self, refresh: bool, limit: usize) -> Vec<GithubItem>;
    fn list_prs(&self, refresh: bool, limit: usize) -> Vec<GithubItem>;
    fn create_pr(&self, input: &DraftPrInput) -> DraftPrOutcome;
    fn pr_status(&self, branch: &str) -> Option<ForgePrStatus>;
    fn pr_merge_state(&self, number: u64, refresh: bool) -> ForgePrMergeStateResult;
    fn merge_pr(&self, number: u64, input: &ForgeMergeInput) -> ForgeMergeResult;
    fn pr_diff(&self, number: u64, refresh: bool) -> ForgePrDiffResult;
    fn comments(&self, kind: GithubItemKind, number: u64, refresh: bool) -> GithubCommentsData;
    fn view_url(&self, kind: ForgeRefKind, reference: &str) -> Option<String>;
    fn list(&self, refresh: bool, limit: usize) -> GithubData;
}
