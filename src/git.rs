//! Root shim for the kernel `git` module.
//!
//! The implementation moved to `tracedecay_runtime_core::git` in the one-shot
//! crate split. This glob keeps every historical `crate::git::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::git::*;
