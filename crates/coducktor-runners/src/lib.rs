//! Backend wire mappers for the normalized UI event protocol.
//!
//! These modules deliberately accept structural JSON rather than strict vendor
//! structs. Agent processes are external and their wire formats evolve; a bad
//! frame must be ignored without taking down the run.

mod wire;

pub mod claude;
pub mod codex;
pub mod opencode;
pub mod pi;
