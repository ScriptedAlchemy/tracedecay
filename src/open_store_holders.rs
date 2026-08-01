//! Root shim for the kernel `open_store_holders` module.
//!
//! The implementation moved to `tracedecay_runtime_core::open_store_holders` in the one-shot
//! crate split. This glob keeps every historical `crate::open_store_holders::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::open_store_holders::*;
