//! Root shim for the kernel `types` module.
//!
//! The implementation moved to `tracedecay_runtime_core::types` in the one-shot
//! crate split. This glob keeps every historical `crate::types::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::types::*;
