use serde_json::Value;

/// One event from an in-process engine topic.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Data {
        topic: String,
        data: Value,
    },
    /// The subscriber fell behind this topic's bounded channel. Consumers must recover from
    /// their durable source instead of pretending the missing payloads were delivered.
    Lagged {
        topic: String,
        count: u64,
    },
}

impl EngineEvent {
    pub fn data(topic: impl Into<String>, data: Value) -> Self {
        Self::Data {
            topic: topic.into(),
            data,
        }
    }

    pub fn topic(&self) -> &str {
        match self {
            Self::Data { topic, .. } | Self::Lagged { topic, .. } => topic,
        }
    }

    pub fn payload(&self) -> Option<&Value> {
        match self {
            Self::Data { data, .. } => Some(data),
            Self::Lagged { .. } => None,
        }
    }
}
