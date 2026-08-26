//! Root shim for the kernel `redundancy` module.
//!
//! The implementation moved to `tracedecay_runtime_core::redundancy` in the one-shot
//! crate split. This glob keeps every historical `crate::redundancy::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::redundancy::*;
