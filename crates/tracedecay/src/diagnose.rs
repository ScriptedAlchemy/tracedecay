//! Diagnostic severity + record types — moved to `tracedecay-usecases::diagnose`.
//!
//! Thin shim so every `crate::diagnose::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::diagnose::*;
