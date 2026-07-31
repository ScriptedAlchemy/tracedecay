//! Root shim for the kernel `store` module.
//!
//! The implementation moved to `tracedecay_runtime_core::store` in the one-shot
//! crate split. This glob keeps every historical `crate::store::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::store::*;
