//! Domain-shaped client boundary for the terminal cockpit.

mod engine;
mod error;
mod http;
mod in_process;
mod scope;
mod sse;
mod ws;

pub use engine::{Engine, StartRunInput, Topic};
pub use error::EngineError;
pub use http::{HttpEngine, RunStreamEvent};
pub use in_process::InProcessEngine;
pub use scope::{API_PREFIX, Scope, api_path, encode_path_segment, is_workspace_route};
pub use sse::SseFrame;
pub use ws::EngineEvent;
