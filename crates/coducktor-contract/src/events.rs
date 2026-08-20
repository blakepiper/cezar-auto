use serde::{Deserialize, Serialize};

use crate::compat::ExtraFields;
use crate::routing::RoutingDecision;

/// The fixed page size.
pub const RUN_HISTORY_PAGE_ITEMS: u64 = 100;

/// The open `RunEvent` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub seq: f64,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// A history cursor accepted by the history and event routes.
pub type RunHistoryCursor = String;

/// The open history event envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryEvent {
    pub seq: f64,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// `RunHistoryPage` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryPage {
    pub events: Vec<RunHistoryEvent>,
    pub item_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<RunHistoryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_cursor: Option<RunHistoryCursor>,
    pub live_cursor: RunHistoryCursor,
    pub as_of_seq: u64,
    pub has_older: bool,
}

/// `RunHistoryContext` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryContext {
    pub context_events: Vec<RunHistoryEvent>,
    pub as_of_seq: u64,
}

/// Normalized automatic-routing selection event. The open event envelope remains the wire
/// compatibility boundary; this typed payload is used by new producers and consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingDecisionEvent {
    pub decision: RoutingDecision,
}
