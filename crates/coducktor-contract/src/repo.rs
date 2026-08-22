use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::health::RepoInfo;

/// `StatusEntry` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEntry {
    pub status: String,
    pub path: String,
}

/// `LogEntry` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub when: String,
}

/// The empty-repository branch of `RepoResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyRepoResponse {
    pub info: Option<RepoInfo>,
    pub status: Vec<Value>,
    pub log: Vec<Value>,
    pub branches: Vec<Value>,
    pub base_branch: Option<String>,
}

/// The repository branch of `RepoResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentRepoResponse {
    pub info: RepoInfo,
    pub status: Vec<StatusEntry>,
    pub log: Vec<LogEntry>,
    pub branches: Vec<String>,
    pub base_branch: Option<String>,
}

/// `RepoResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RepoResponse {
    Empty(EmptyRepoResponse),
    Present(PresentRepoResponse),
}

/// `RepoBranchResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoBranchResponse {
    pub branch: String,
    pub created: bool,
}

/// The request body for creating or checking out a repository branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoBranchRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The module-local diff-stat shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoDiffStat {
    pub adds: f64,
    pub dels: f64,
    pub files: f64,
}

/// `ChangedFile` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: ChangedFileStatus,
    pub adds: f64,
    pub dels: f64,
    pub binary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<bool>,
    pub patch: String,
}

/// Changed-file status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangedFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

/// `ChangesPayload` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesPayload {
    pub files: Vec<ChangedFile>,
    pub stat: RepoDiffStat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repointed_head: Option<RepointedHead>,
}

/// The additive head-repoint metadata in `ChangesPayload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepointedHead {
    pub head_branch: String,
    pub task_branch: String,
}

/// `RepoCommitPayload` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoCommitPayload {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub when: String,
    pub files: Vec<ChangedFile>,
    pub stat: RepoDiffStat,
}

/// `WorktreeDirEntry` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: WorktreeEntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

/// The `type` discriminator in a worktree directory row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeEntryType {
    Dir,
    File,
}

/// `WorktreeEntry` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WorktreeEntry {
    Dir {
        path: String,
        entries: Vec<WorktreeDirEntry>,
    },
    File {
        path: String,
        size: f64,
        binary: bool,
        #[serde(rename = "tooLarge")]
        too_large: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
}

/// Mirrors the lifecycle enum used by `WorktreeInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeRunStatus {
    Queued,
    Running,
    Idle,
    Waiting,
    Review,
    Done,
    Failed,
    Cancelled,
}

/// `WorktreeInfo` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    pub run_id: String,
    pub title: String,
    pub status: WorktreeRunStatus,
    pub branch: Option<String>,
    pub size_bytes: Option<f64>,
    pub finished_at: Option<String>,
    pub reclaimable: bool,
}

/// `WorktreesResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreesResponse {
    pub worktrees: Vec<WorktreeInfo>,
    pub total_bytes: Option<u64>,
    pub keep: u64,
}

/// `ReclaimWorktreesResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimWorktreesResponse {
    pub reclaimed: Vec<String>,
}
