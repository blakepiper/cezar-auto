//! The workflow catalog. Mirrors the file-loading half of `packages/cezar/src/workflows/`
//! — see `load`'s and `types`'s module docs for exactly what's ported at this step versus
//! what's deferred to B6 (`workflows::run`, the `RunManager`).

pub mod load;
pub mod types;
