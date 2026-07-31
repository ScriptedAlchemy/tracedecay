//! Root shim for the kernel `serde_util` module.
//!
//! The implementation moved to `tracedecay_runtime_core::serde_util` in the one-shot
//! crate split. This glob keeps every historical `crate::serde_util::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::serde_util::*;
