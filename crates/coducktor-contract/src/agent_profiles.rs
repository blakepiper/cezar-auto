use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::health::Runner;
use crate::workspace::ProviderStatus;

/// The reserved discovered-account id.
pub const DEFAULT_AGENT_ACCOUNT_ID: &str = "default";

/// `AgentAccountFile` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAccountFile {
    pub id: String,
    pub label: String,
    pub path: String,
    pub exists: bool,
}

/// `AgentProfile` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub provider: Runner,
    pub label: String,
    pub config_dir: String,
    pub path: String,
    pub exists: bool,
    pub looks_valid: bool,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProviderStatus>,
    pub files: Vec<AgentAccountFile>,
}

/// Addresses an account in the per-account routes.
pub fn agent_account_route_id(profile: &AgentProfile) -> String {
    if profile.is_default {
        format!("default:{}", runner_name(profile.provider))
    } else {
        profile.id.clone()
    }
}

fn runner_name(runner: Runner) -> &'static str {
    match runner {
        Runner::Claude => "claude",
        Runner::Codex => "codex",
        Runner::OpenCode => "opencode",
        Runner::Pi => "pi",
    }
}

/// `AgentAccountSelection` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentAccountSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi: Option<String>,
}

/// `AgentProfilesResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfilesResponse {
    pub editable: bool,
    pub profiles: Vec<AgentProfile>,
    pub profile_capable_providers: Vec<Runner>,
    pub selections: BTreeMap<String, AgentAccountSelection>,
    pub defaults: AgentAccountSelection,
}

/// `SelectAgentProfileInput` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectAgentProfileInput {
    pub project_id: Option<String>,
    pub provider: Runner,
    pub profile_id: Option<String>,
}

/// `AgentProfileSelectionsResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileSelectionsResponse {
    pub selections: BTreeMap<String, AgentAccountSelection>,
    pub defaults: AgentAccountSelection,
}

/// `AgentAccountStatusResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAccountStatusResponse {
    pub status: ProviderStatus,
}

/// Mirrors the labelled detail field in `AgentAccountDetailsResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAccountDetailField {
    pub label: String,
    pub value: String,
}

/// `AgentAccountDetailsResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAccountDetailsResponse {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub fields: Vec<AgentAccountDetailField>,
}

/// `OpenAgentAccountFileInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAgentAccountFileInput {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `OpenAgentAccountFileResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAgentAccountFileResponse {
    pub opened: bool,
    pub path: String,
}

/// `CreateAgentProfileInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentProfileInput {
    pub provider: Runner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub config_dir: String,
}

/// `AgentProfileResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProfileResponse {
    pub profile: AgentProfile,
}

/// `UpdateAgentProfileInput` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAgentProfileInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
}

/// `RemoveAgentProfileResponse` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveAgentProfileResponse {
    pub removed: bool,
    pub id: String,
}
