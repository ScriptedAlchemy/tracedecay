//! Graph query manager + node metrics — moved to `tracedecay-usecases::graph::queries`.
//!
//! Thin shim so every `crate::graph/queries::…` path in the root crate keeps
//! resolving after the crate split. See the canonical module for rationale.

pub use tracedecay_usecases::graph::queries::*;
