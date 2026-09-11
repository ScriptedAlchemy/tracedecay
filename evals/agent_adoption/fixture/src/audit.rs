//! Audit-tier fixture surface.
//!
//! This module intentionally plants a few ship-risk markers so the
//! audit/safety-scan and dead-code scenarios have something
//! concrete and unambiguous to find. It is deliberately kept OUT of the order
//! flow (`orders`/`pricing`/`inventory`/`discount`) so the exploration,
//! call-tracing, and impact scenarios' ground truth is unaffected.
//!
//! Planted markers:
//!   * a `TODO` marker (for TODO/audit scans),
//!   * a needless `unsafe` block (for audit-safety / panic-and-risk scans).

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
