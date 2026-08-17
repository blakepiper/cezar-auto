//! Backend wire mappers for the normalized UI event protocol.
//!
//! These modules deliberately accept structural JSON rather than strict vendor
//! structs. Agent processes are external and their wire formats evolve; a bad
//! frame must be ignored without taking down the run.

mod wire;

pub mod agent_env;
pub mod agent_runner;
pub mod child_process;
pub mod claude;
pub mod claude_runner;
pub mod codex;
pub mod codex_runner;
pub mod model_identity;
pub mod opencode;
pub mod opencode_runner;
pub mod pi;
pub mod pi_runner;
pub mod session_factory;
pub mod usage;
pub mod v1_text_coalescer;
