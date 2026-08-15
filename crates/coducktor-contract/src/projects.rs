use serde::{Deserialize, Serialize};

use crate::health::ForgeKind;

/// Mirrors `packages/contract/src/projects.ts::ProjectListEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListEntry {
    pub id: String,
    pub name: String,
    pub root: String,
    pub added_at: String,
    pub last_opened_at: String,
    pub source: ProjectSource,
    pub status: ProjectStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// The project source discriminator from `packages/contract/src/projects.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectSource {
    Local,
    Checkout,
}

/// The project health discriminator from `packages/contract/src/projects.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectStatus {
    Ok,
    Missing,
    NotGit,
}

/// Mirrors `packages/contract/src/projects.ts::ProjectsResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectListEntry>,
    pub boot_project: String,
    pub projects_dir: String,
}

/// Mirrors `packages/contract/src/projects.ts::RegisterProjectResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterProjectResponse {
    pub project: ProjectListEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Mirrors `packages/contract/src/projects.ts::RemoveProjectResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveProjectResponse {
    pub removed: bool,
    pub id: String,
}

/// Mirrors `packages/contract/src/projects.ts::UpdateProjectResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectResponse {
    pub project: ProjectListEntry,
}

/// The maximum length of one project tag.
pub const PROJECT_TAG_MAX_LENGTH: usize = 32;

/// The maximum number of tags on one project.
pub const PROJECT_TAGS_MAX: usize = 20;

/// Mirrors `packages/contract/src/projects.ts::UpdateProjectInput`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Option<Vec<String>>>,
}

/// Mirrors `packages/contract/src/projects.ts::CheckoutProjectInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutProjectInput {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
}

/// Mirrors `packages/contract/src/projects.ts::FsBrowseDir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsBrowseDir {
    pub name: String,
    pub path: String,
    pub is_repo: bool,
}

/// Mirrors `packages/contract/src/projects.ts::FsBrowseResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsBrowseResponse {
    pub path: String,
    pub parent: Option<String>,
    pub dirs: Vec<FsBrowseDir>,
    pub truncated: bool,
}

/// Mirrors `packages/contract/src/projects.ts::LaunchKeyResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchKeyResponse {
    pub key: String,
}
