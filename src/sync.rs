//! Root shim for the kernel `sync` module.
//!
//! The implementation moved to `tracedecay_runtime_core::sync` in the one-shot
//! crate split. This glob keeps every historical `crate::sync::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::sync::*;
