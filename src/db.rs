//! Root shim for the kernel `db` module.
//!
//! The implementation moved to `tracedecay_runtime_core::db` in the one-shot
//! crate split. This glob keeps every historical `crate::db::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::db::*;
