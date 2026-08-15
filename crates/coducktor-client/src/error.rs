use std::fmt;

/// Errors exposed to screens. Transport-specific status codes never cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    NotFound,
    Conflict { reason: String },
    Unavailable { reason: String },
    Transport(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("resource not found"),
            Self::Conflict { reason } => write!(formatter, "conflict: {reason}"),
            Self::Unavailable { reason } => write!(formatter, "service unavailable: {reason}"),
            Self::Transport(reason) => write!(formatter, "transport failure: {reason}"),
        }
    }
}

impl std::error::Error for EngineError {}
