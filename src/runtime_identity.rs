//! Root shim for the kernel `runtime_identity` module.
//!
//! The implementation moved to `tracedecay_runtime_core::runtime_identity` in the one-shot
//! crate split. This glob keeps every historical `crate::runtime_identity::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::runtime_identity::*;
