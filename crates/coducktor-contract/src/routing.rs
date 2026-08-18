//! Persisted, provider-neutral shapes for automatic routing.
//!
//! These types deliberately contain only sanitized identities and bounded reason codes. Runner
//! protocol payloads and provider error bodies stop at the runner seam.

use serde::{Deserialize, Serialize};

use crate::health::{Runner, RunnerSelection};
use crate::reasoning::{ConcreteReasoningEffort, ReasoningEffort};

/// The authored picker intent for an automatic step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingIntent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// The concrete route selected for one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSelection {
    pub runner: Runner,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ConcreteReasoningEffort>,
    pub route_key: String,
}

/// A compact task-profile summary kept with a decision for explanation and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskProfileSummary {
    pub kind: String,
    pub quality_floor: String,
    pub complexity: u8,
    pub risk: u8,
    pub breadth: u8,
    pub ambiguity: u8,
}

/// Stable reason codes used in decisions and considered-candidate details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingReasonCode {
    Selected,
    Disabled,
    Missing,
    Disconnected,
    UnsupportedImages,
    UnsupportedTools,
    UnsupportedContext,
    UnsupportedReasoning,
    ReservedQuota,
    HardExhausted,
    ConcurrencyFull,
    UnknownUsage,
    AlreadyAttempted,
    PinnedConflict,
    NoCapableRoute,
}

/// One candidate retained for a bounded decision explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsideredCandidate {
    pub route_key: String,
    pub runner: Runner,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub eligible: bool,
    pub reason: RoutingReasonCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
}

/// The deterministic outcome of one routing evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingDecision {
    pub task_profile: TaskProfileSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<RouteSelection>,
    pub considered: Vec<ConsideredCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
    pub generation: u64,
}

/// Why a step is waiting for routing capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingWaitReason {
    Capacity,
    NoCapableRoute,
    RefreshingUsage,
}

/// A bounded route hold included in a durable wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedRoute {
    pub route_key: String,
    pub reason: RoutingReasonCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
}

/// Durable capacity wait state. Legacy `autoResumeAt` remains readable beside this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingWait {
    pub reason: RoutingWaitReason,
    pub generation: u64,
    pub attempted_routes: Vec<String>,
    pub blocked_routes: Vec<BlockedRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
    pub created_at: String,
    pub last_checked_at: String,
    pub attempts: u32,
}

/// One confirmed or pending automatic route attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingAttempt {
    pub route_key: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}
