use serde::{Deserialize, Serialize};

/// `ReasoningEffort` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Auto,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

/// `ConcreteReasoningEffort` contract shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConcreteReasoningEffort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}
