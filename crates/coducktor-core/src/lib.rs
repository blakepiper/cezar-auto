//! Durable state, workflows, skills, Git helpers, and run lifecycle for Coducktor.
//!
//! This crate is the source of truth for the local file and workflow behavior used by the
//! in-process engine and terminal UI. All writes preserve the repository's compatibility and
//! atomicity rules; presentation and agent-wire details stay in their owning crates.

pub mod config;
pub mod git;
pub mod handoff;
pub mod paths;
pub mod runs;
pub mod skills;
pub mod time;
pub mod workflows;
pub mod workspace;
mod zod;
