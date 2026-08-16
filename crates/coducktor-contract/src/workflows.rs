use serde::{Deserialize, Serialize};

use crate::health::RunnerSelection;

fn default_retry_max() -> u32 {
    2
}

/// Mirrors `packages/contract/src/workflows.ts::WorkflowStepDef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepDef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash_allowlist: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<WorkflowOnFail>,
}

/// The retry policy nested in a workflow step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOnFail {
    pub retry: String,
    #[serde(default = "default_retry_max")]
    pub max: u32,
}

/// Mirrors `packages/contract/src/workflows.ts::WorkflowDef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<WorkflowStepDef>,
    pub source: WorkflowSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The source discriminator in a workflow catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowSource {
    BuiltIn,
    File,
}

/// Mirrors `packages/contract/src/workflows.ts::WorkflowLoadIssue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLoadIssue {
    pub path: String,
    pub message: String,
}

/// Mirrors `packages/contract/src/workflows.ts::WorkflowsResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsResponse {
    pub workflows: Vec<WorkflowDef>,
    pub issues: Vec<WorkflowLoadIssue>,
}

/// Mirrors `packages/contract/src/workflows.ts::SaveWorkflowInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveWorkflowInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<WorkflowStepDef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<bool>,
}

/// Mirrors `packages/contract/src/workflows.ts::SaveWorkflowResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveWorkflowResponse {
    pub path: String,
    pub name: String,
}

/// Mirrors `packages/contract/src/workflows.ts::ParsedWorkflow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedWorkflow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<WorkflowStepDef>,
}

/// The `POST /workflows/parse` request body (`parseWorkflowSchema` in `server.ts`) — the
/// builder's import path: pasted YAML in either form, parsed and normalized server-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseWorkflowInput {
    pub yaml: String,
}

/// The planning prompt accepted by `POST /api/v1/plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInput {
    pub task: String,
}

/// Mirrors `packages/contract/src/workflows.ts::DeleteWorkflowResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteWorkflowResponse {
    pub ok: bool,
    pub path: String,
}

/// Mirrors `packages/contract/src/workflows.ts::PlanResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub steps: Vec<WorkflowStepDef>,
    pub rationale: String,
    pub fallback: bool,
}
