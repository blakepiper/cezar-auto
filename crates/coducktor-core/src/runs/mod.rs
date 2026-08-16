//! The per-project run store's file layer: `.ai/coducktor/runs.json`, the per-run NDJSON
//! event log under `.ai/coducktor/runs/`, and count-based retention. Mirrors
//! `packages/cezar/src/runs/{store,retention}.ts` — see `store`'s module doc for exactly
//! which slice of `store.ts` this crate owns at this step versus what's deferred to B3/B6.

pub mod events;
pub mod retention;
pub mod store;
