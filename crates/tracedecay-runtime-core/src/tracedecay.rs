//! Kernel-owned slice of the root `tracedecay` orchestrator module.
//!
//! Only the shared wall-clock reader moved: `db::memory_v2`, `memory::store`,
//! and `runtime_identity` all stamp records with it, and those layers now live
//! in this crate. The root `tracedecay` module re-exports it so
//! `crate::tracedecay::current_timestamp` keeps resolving on both sides of the
//! split.

/// Returns the current UNIX timestamp in seconds.
pub fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
