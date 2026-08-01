//! Root shim for the kernel `path_scope` module.
//!
//! The implementation moved to `tracedecay_runtime_core::path_scope` in the one-shot
//! crate split. This glob keeps every historical `crate::path_scope::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::path_scope::*;
