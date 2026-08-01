//! Compatibility shim: the durability journal moved into the kernel so the
//! global-db crate no longer needs a dependency on this migration crate.
pub use tracedecay_runtime_core::durability::*;
