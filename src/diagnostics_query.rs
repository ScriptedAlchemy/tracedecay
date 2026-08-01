//! Diagnostics query types — moved to `tracedecay-usecases::diagnostics_query`.
//!
//! Thin shim so every `crate::diagnostics_query::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::diagnostics_query::*;
