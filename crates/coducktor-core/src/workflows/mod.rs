//! The workflow catalog and execution foundation. Mirrors the file-loading half of
//! `packages/coducktor/src/workflows/` — see `load`, `types`, and the focused
//! `run::{lifecycle,session,recovery,review_gate,auto_resume,context_refresh,variants,quota,semaphore}`
//! modules for the deliberately narrow B6 slice in this crate.

pub mod load;
pub mod run;
pub mod types;
