use serde::{Deserialize, Serialize};

use crate::health::Runner;

/// `AgentConfigFormat` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentConfigFormat {
    Json,
    #[serde(rename = "jsonc")]
    JsonC,
    Toml,
    Markdown,
}

/// `AgentConfigScope` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentConfigScope {
    User,
    Project,
    Local,
}

/// `AgentConfigKind` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentConfigKind {
    Settings,
    Memory,
    Mcp,
}

/// `AgentConfigTracked` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentConfigTracked {
    Tracked,
    Gitignored,
    OutsideRepo,
}

/// `AgentConfigFile` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigFile {
    pub id: String,
    pub runners: Vec<Runner>,
    pub kind: AgentConfigKind,
    pub scope: AgentConfigScope,
    pub label: String,
    pub path: String,
    pub format: AgentConfigFormat,
    pub tracked: AgentConfigTracked,
    pub seeded: bool,
    pub holds_mcp: bool,
    pub precedence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hot_reload: Option<String>,
    pub docs_url: String,
    pub exists: bool,
    pub size: f64,
    pub version: Option<String>,
    pub writable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<String>,
}

/// `UserMcpListing` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMcpListing {
    pub path: String,
    pub servers: Vec<String>,
    pub readable: bool,
}

/// `AgentConfigListing` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigListing {
    pub editable: bool,
    pub files: Vec<AgentConfigFile>,
    pub user_mcp: Option<UserMcpListing>,
}

/// `AgentConfigFileContent` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigFileContent {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub content: String,
    pub version: Option<String>,
}

/// `SetAgentConfigInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAgentConfigInput {
    pub content: String,
    pub version: Option<String>,
}
