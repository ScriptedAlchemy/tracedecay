//! Root shim for the kernel `memory` module.
//!
//! The implementation moved to `tracedecay_runtime_core::memory` in the one-shot
//! crate split. This glob keeps every historical `crate::memory::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::memory::*;
