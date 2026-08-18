//! Serde representations for persisted state, runner requests, and normalized UI data.
//!
//! This crate owns the compatible shapes shared by the core, client, runner, forge, and TUI
//! crates. Legacy fields remain where they are needed to read existing state.

pub mod agent_config;
pub mod agent_profiles;
pub mod compat;
pub mod events;
pub mod github;
pub mod health;
pub mod ide;
pub mod projects;
pub mod reasoning;
pub mod repo;
pub mod runs;
pub mod scratchpad;
pub mod skills;
pub mod workflows;
pub mod workspace;

pub use agent_config::*;
pub use agent_profiles::*;
pub use events::*;
pub use github::*;
pub use health::*;
pub use ide::*;
pub use projects::*;
pub use reasoning::*;
pub use repo::*;
pub use runs::*;
pub use scratchpad::*;
pub use skills::*;
pub use workflows::*;
pub use workspace::*;
