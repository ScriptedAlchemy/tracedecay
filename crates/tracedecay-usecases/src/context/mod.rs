//! Code-index-backed source-read helpers (`source_read`, `read_modes`,
//! `markdown_sections`) consumed by `primitives::concrete`, composed with the
//! request-context value types and read cache that moved to
//! `tracedecay-session-memory`.
//!
//! RE-EXPORT SEAM: the glob keeps every old `tracedecay_usecases::context::…`
//! path valid until a later slice re-points the remaining consumers at
//! `tracedecay_session_memory::context` and deletes it.

pub mod markdown_sections;
pub mod read_modes;
pub mod source_read;

pub use tracedecay_session_memory::context::*;
