use serde::{Deserialize, Serialize};

use crate::compat::ExtraFields;

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

/// `RunIdParam` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdParam {
    pub id: String,
}

/// A history cursor accepted by the history and event routes.
pub type RunHistoryCursor = String;

/// `RunHistoryQuery` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHistoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<RunHistoryCursor>,
}

/// `RunEventsQuery` contract shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEventsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<RunHistoryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<f64>,
}

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

/// The checkout phase enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckoutPhase {
    Cloning,
    Done,
    Error,
}

/// `CheckoutProgressEvent` contract shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutProgressEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    pub name: String,
    pub phase: CheckoutPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
