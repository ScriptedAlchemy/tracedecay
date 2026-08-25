//! Root shim for the kernel `errors` module.
//!
//! The implementation moved to `tracedecay_runtime_core::errors` in the one-shot
//! crate split. This glob keeps every historical `crate::errors::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::errors::*;
