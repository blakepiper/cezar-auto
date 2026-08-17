use serde_json::Value;

/// One event from an in-process engine topic.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineEvent {
    pub topic: String,
    pub data: Value,
}
