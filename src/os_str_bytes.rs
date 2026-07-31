//! Root shim for the kernel `os_str_bytes` module.
//!
//! The implementation moved to `tracedecay_runtime_core::os_str_bytes` in the one-shot
//! crate split. This glob keeps every historical `crate::os_str_bytes::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::os_str_bytes::*;
