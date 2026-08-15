//! Markdown section preview/handle/structure — moved to
//! `tracedecay-usecases::context::markdown_sections`.
//!
//! Thin shim so `crate::context::markdown_sections::…` paths in the root crate
//! resolve after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::context::markdown_sections::*;
