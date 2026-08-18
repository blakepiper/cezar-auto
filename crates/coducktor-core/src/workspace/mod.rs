//! `~/.coducktor/` — the per-user workspace: config and project registry, global UI state,
//! migrations, and agent accounts.

pub mod agent_accounts;
pub mod config;
pub mod migrations;
pub mod projects;
pub mod provider_usage;
pub mod scratchpad;
pub mod ui_state;
