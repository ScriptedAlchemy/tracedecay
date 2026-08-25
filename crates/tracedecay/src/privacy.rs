//! Root shim for the kernel `privacy` module.
//!
//! The implementation moved to `tracedecay_runtime_core::privacy` in the one-shot
//! crate split. This glob keeps every historical `crate::privacy::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::privacy::*;
