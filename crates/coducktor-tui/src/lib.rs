//! The Rust TUI's modules, as a library crate.
//!
//! `main.rs` is the `coducktor` binary's entry point and stays thin; everything
//! reusable — the app state machine, screens, widgets, and the rendering primitives
//! benches and future integration tests need to link against — lives here.

pub mod app;
pub mod cli;
pub mod diff;
pub mod image;
pub mod input;
pub mod markdown;
pub mod new_task_form;
pub mod notify;
pub mod overlay;
pub mod screens;
pub mod service;
pub mod skills;
pub mod terminal;
pub mod theme;
pub mod widgets;
