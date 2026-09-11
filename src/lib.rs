//! jrs — a Java build system, in Rust.
//!
//! Library-first by design (SPEC §6): every phase of a build lives in a module
//! that can be exercised without spawning the CLI, and `main.rs` is nothing but
//! argument parsing and an exit code.
//!
//! The dependency arrows all point one way. `ui` knows nothing about builds;
//! `manifest`, `project` and `resolve` know nothing about terminals beyond
//! reporting progress into shared state; `cli` is the only module that decides
//! what a user sees.

pub mod cli;
pub mod compile;
pub mod completions;
pub mod config;
pub mod dist;
pub mod edit;
pub mod error;
pub mod image;
pub mod lockfile;
pub mod manifest;
pub mod migrate;
pub mod native_image;
pub mod obfuscate;
pub mod package;
pub mod project;
pub mod resolve;
pub mod runner;
pub mod task;
pub mod test;
pub mod test_report;
pub mod toolchain;
pub mod ui;

pub mod json;
pub mod model;
pub mod timings;

pub use error::{JrsError, Result};
