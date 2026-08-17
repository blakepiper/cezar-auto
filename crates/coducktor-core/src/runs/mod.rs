//! The per-project run store's file layer: `.ai/coducktor/runs.json`, the per-run NDJSON event
//! log under `.ai/coducktor/runs/`, and count-based retention.

pub mod ask;
pub mod events;
pub mod retention;
pub mod store;
pub mod task_markers;
pub mod task_refs;
