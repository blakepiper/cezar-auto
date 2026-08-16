//! `~/.coducktor/` — the per-user workspace: config + project registry, global UI state,
//! migrations, and agent accounts. Mirrors `packages/cezar/src/workspace/`.

pub mod agent_accounts;
pub mod config;
pub mod migrations;
pub mod ui_state;
