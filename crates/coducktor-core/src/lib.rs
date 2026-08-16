//! The on-disk file layer ported from `packages/cezar/src/{paths,config,workspace}.ts`.
//!
//! The source of truth for each module's behavior remains the matching TypeScript file —
//! every public item cites it in a doc comment. `packages/cezar` keeps running the server
//! until Phase B's cutover (B9/B11), so this crate and the TS tree read and write the
//! SAME on-disk files for a while; keeping them byte-compatible is the whole point (see
//! `paths::coducktor_home_dir`'s doc comment for the sharpest example of what that costs).

pub mod config;
pub mod git;
pub mod handoff;
pub mod paths;
pub mod runs;
pub mod skills;
pub mod time;
pub mod todos;
pub mod workflows;
pub mod workspace;
mod zod;
