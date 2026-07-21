//! Shared CLI presentation contracts.
//!
//! These helpers preserve the application's canonical problem and cursor
//! contracts while leaving transport-specific rendering to each surface.

pub mod json;
pub mod markdown;
pub mod problem;

pub use tracedecay::daemon_client::{canonical_cursor, canonical_problem_kind};
