//! Kernel store-runtime re-export kept at the composition root.
//!
//! The session registry now lives in `tracedecay-store-runtime`. This module
//! still forwards the kernel `store_runtime` surface (`registry`, `shard`,
//! `telemetry`) so existing daemon callers of
//! `crate::daemon::store_runtime::registry` keep a single path. See
//! `tracedecay_runtime_core`'s crate-level doc.

pub(crate) use tracedecay_runtime_core::store_runtime::*;
