//! Diagnostics store — moved to `tracedecay-usecases::diagnostics_store`.
//!
//! Thin shim so every `crate::diagnostics_store::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::diagnostics_store::*;
