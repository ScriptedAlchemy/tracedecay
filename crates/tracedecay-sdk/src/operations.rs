//! Typed public operation descriptors.
//!
//! Rendered at build time by `build.rs` from the canonical application
//! registry. Nothing here is checked in; edit `src/codegen.rs` instead.

include!(concat!(env!("OUT_DIR"), "/operations.rs"));
