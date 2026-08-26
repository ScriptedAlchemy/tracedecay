//! Root shim for the kernel `sqlite_read_snapshot` module.
//!
//! The implementation moved to `tracedecay_runtime_core::sqlite_read_snapshot` in the one-shot
//! crate split. This glob keeps every historical `crate::sqlite_read_snapshot::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::sqlite_read_snapshot::*;
