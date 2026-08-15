//! Serde representations of the surviving HTTP contract.
//!
//! The source of truth for each wire shape remains the matching TypeScript module under
//! `packages/contract/src/`. This crate deliberately does not include the deleted automation
//! contract; the old run provenance stamp is defined in `runs` instead.

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
pub use skills::*;
pub use workflows::*;
pub use workspace::*;
