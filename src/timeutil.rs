//! Root shim for the kernel `timeutil` module.
//!
//! The implementation moved to `tracedecay_runtime_core::timeutil` in the one-shot
//! crate split. This glob keeps every historical `crate::timeutil::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::timeutil::*;
