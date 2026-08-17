//! The workflow catalog and execution foundation. The loader owns user-authored workflow files;
//! the run module owns execution state, sessions, review gates, and related lifecycle helpers.

pub mod load;
pub mod run;
pub mod types;
