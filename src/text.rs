//! Root shim for the kernel `text` module.
//!
//! The implementation moved to `tracedecay_runtime_core::text` in the one-shot
//! crate split. This glob keeps every historical `crate::text::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::text::*;
