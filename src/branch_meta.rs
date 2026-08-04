//! Root shim for the kernel `branch_meta` module.
//!
//! The implementation moved to `tracedecay_runtime_core::branch_meta` in the one-shot
//! crate split. This glob keeps every historical `crate::branch_meta::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::branch_meta::*;
