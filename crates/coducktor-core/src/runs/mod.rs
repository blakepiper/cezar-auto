//! The per-project run store's file layer: `.ai/coducktor/runs.json`, the per-run NDJSON
//! event log under `.ai/coducktor/runs/`, and count-based retention. Mirrors
//! `packages/coducktor/src/runs/{store,retention}.ts` — see `store`'s module doc for exactly
//! which slice of `store.ts` this crate owns at this step versus what's deferred to B3/B6.

pub mod ask;
pub mod events;
pub mod retention;
pub mod store;
pub mod task_markers;
pub mod task_refs;
