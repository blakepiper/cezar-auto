use serde::{Deserialize, Serialize};

/// Mirrors `packages/contract/src/health.ts::Runner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    Claude,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
}

/// Mirrors `packages/contract/src/health.ts::RunnerSelection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerSelection {
    Claude,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
    Auto,
}

/// Mirrors `packages/contract/src/health.ts::RepoInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub root: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// The backend name allowlist from `packages/contract/src/health.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendCheckName {
    Claude,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
    Gh,
    Git,
}

/// Mirrors `packages/contract/src/health.ts::BackendCheck`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCheck {
    pub name: BackendCheckName,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Mirrors `packages/contract/src/health.ts::ForgeInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeInfo {
    pub kind: ForgeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The only forge literal currently accepted by the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForgeKind {
    #[serde(rename = "github")]
    GitHub,
}

/// Mirrors `packages/contract/src/health.ts::Capabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub followups: bool,
}

/// A health response from `packages/contract/src/health.ts::HealthResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    pub repo_root: String,
    pub repo: Option<RepoInfo>,
    pub checks: Vec<BackendCheck>,
    pub default_runner: RunnerSelection,
    pub forge: Option<ForgeInfo>,
    pub capabilities: Capabilities,
    pub projects: Vec<HealthProject>,
    pub boot_project: String,
}

/// The compact project pair embedded in the health response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthProject {
    pub id: String,
    pub name: String,
}
