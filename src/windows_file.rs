//! Root shim for the kernel `windows_file` module.
//!
//! The implementation moved to `tracedecay_runtime_core::windows_file` in the one-shot
//! crate split. This glob keeps every historical `crate::windows_file::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::windows_file::*;
