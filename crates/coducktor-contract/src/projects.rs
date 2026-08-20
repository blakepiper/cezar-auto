use serde::{Deserialize, Serialize};

use crate::health::ForgeKind;

/// `ProjectListEntry` contract shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// The project source discriminator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectSource {
    #[default]
    Local,
    Checkout,
}

/// The project health discriminator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectStatus {
    #[default]
    Ok,
    Missing,
    NotGit,
}

/// `ProjectsResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectListEntry>,
    pub boot_project: String,
    pub projects_dir: String,
}

/// `RegisterProjectInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterProjectInput {
    pub root: String,
}

/// `RegisterProjectResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterProjectResponse {
    pub project: ProjectListEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `RemoveProjectResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveProjectResponse {
    pub removed: bool,
    pub id: String,
}

/// `UpdateProjectResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectResponse {
    pub project: ProjectListEntry,
}

/// The maximum length of one project tag.
pub const PROJECT_TAG_MAX_LENGTH: usize = 32;

/// The maximum number of tags on one project.
pub const PROJECT_TAGS_MAX: usize = 20;

/// `UpdateProjectInput` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Option<Vec<String>>>,
}
