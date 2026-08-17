//! The diff engine: parsing, word-level intra-line diff, syntax highlighting, and row layout
//! for the Changes/Files/Commits tabs, repo Git, and compare.

pub mod highlight;
pub mod parse_patch;
pub mod render;
pub mod word_diff;

pub use highlight::Highlighter;
pub use parse_patch::ContextGap;
pub use render::{
    DiffMode, DiffRowAction, DiffViewState, effective_mode, file_key, materialize_gap, render_files,
};
