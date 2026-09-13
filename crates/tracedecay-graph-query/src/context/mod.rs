//! Code-index-backed source-read helpers (`source_read`, `read_modes`,
//! `markdown_sections`) consumed by [`crate::VerifiedGraphQuery`] and the
//! usecases source primitives. The request-context value types and read
//! cache these compose with live in `tracedecay_session_memory::context`.

pub mod markdown_sections;
pub mod read_modes;
pub mod source_read;
