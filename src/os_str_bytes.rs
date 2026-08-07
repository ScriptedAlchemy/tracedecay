//! Root shim for the kernel `os_str_bytes` module.
//!
//! The implementation moved to `tracedecay_runtime_core::os_str_bytes` in the one-shot
//! crate split. This glob keeps every historical `crate::os_str_bytes::…` path resolving
//! from the root crate.

// Nothing in this crate currently calls through the shim, but it is kept
// live so external and future in-crate consumers keep resolving
// `crate::os_str_bytes::…` without needing to know about the crate split.
#[allow(unused_imports)]
pub use tracedecay_runtime_core::os_str_bytes::*;
