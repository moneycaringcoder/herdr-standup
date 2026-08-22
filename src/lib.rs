//! standup — a daily digest of what your agents actually did.
//!
//! The crate is a library plus a thin binary so the integration tests in
//! `tests/` can reach the real modules. A binary-only crate would hide them
//! behind `#[path]` includes, which break as soon as a module says `crate::`.

pub mod clock;
pub mod compare;
pub mod config;
pub mod git;
pub mod herdr;
pub mod model;
pub mod render;
pub mod standup;
pub mod window;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
