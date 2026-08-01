//! Protected-change planning types — moved to `tracedecay-usecases::config::scope_control`.
//!
//! Thin shim so every `crate::config/scope_control::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::config::scope_control::*;
