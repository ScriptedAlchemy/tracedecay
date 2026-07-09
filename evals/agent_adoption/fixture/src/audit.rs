//! Audit-tier fixture surface.
//!
//! This module intentionally plants a few ship-risk markers so the
//! audit/safety-scan, dead-code, and unused-import scenarios have something
//! concrete and unambiguous to find. It is deliberately kept OUT of the order
//! flow (`orders`/`pricing`/`inventory`/`discount`) so the exploration,
//! call-tracing, and impact scenarios' ground truth is unaffected.
//!
//! Planted markers:
//!   * an unused `use` import (for unused-imports / clean-dead-code),
//!   * a `TODO` marker (for TODO/audit scans),
//!   * a needless `unsafe` block (for audit-safety / panic-and-risk scans).

// Planted unused import: `BTreeMap` is referenced nowhere in the crate, so the
// unused-imports scan flags it. (A type that other modules DO use, like
// HashMap, would share one import node and read as "used", so pick a unique
// one.)
use std::collections::BTreeMap;

/// Reinterpret a total (in cents) as a `usize` through a raw-pointer read.
///
/// There is no memory-safety reason for this to use `unsafe` — a plain
/// `total as usize` would do — which is exactly the kind of needless `unsafe`
/// a safety audit is meant to flag.
pub fn raw_total_len(total: u64) -> usize {
    // TODO(audit-fixture): drop this needless `unsafe` in favor of a checked cast.
    let ptr = &total as *const u64;
    unsafe { *ptr as usize }
}
