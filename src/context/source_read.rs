//! Mode-aware source read + indexed path resolution — moved to
//! `tracedecay-usecases::context::source_read`.
//!
//! Thin shim so every `crate::context::source_read::…` path in the root crate
//! keeps resolving after the crate split. See the canonical module for
//! rationale. The canonical `SourceReadOutput` carries the `mode` field the
//! root `tracedecay_read` handler echoes back to callers.

pub use tracedecay_usecases::context::source_read::*;
