//! The diff engine (spec §7.6, §10 A9): parsing, word-level intra-line diff, syntax
//! highlighting and row layout for the Changes/Files/Commits tabs, repo git, and compare.
//! Replaces `web/src/components/diff/{parse-patch,word-diff,diff-view,diff-scroll}.ts(x)`.

pub mod highlight;
pub mod parse_patch;
pub mod render;
pub mod word_diff;

pub use highlight::Highlighter;
pub use parse_patch::ContextGap;
pub use render::{
    DiffMode, DiffRowAction, DiffViewState, effective_mode, file_key, materialize_gap, render_files,
};
