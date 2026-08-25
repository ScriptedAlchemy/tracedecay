//! Root shim for the kernel `lifecycle_lease` module.
//!
//! The implementation moved to `tracedecay_runtime_core::lifecycle_lease` in the one-shot
//! crate split. This glob keeps every historical `crate::lifecycle_lease::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::lifecycle_lease::*;
