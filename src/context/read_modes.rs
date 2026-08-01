//! Source read-mode + line-range types — moved to `tracedecay-usecases::context::read_modes`.
//!
//! Thin shim so every `crate::context/read_modes::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::context::read_modes::*;
