//! Kernel-owned slice of the root `tracedecay` orchestrator module.
//!
//! The shared wall-clock reader moved with `runtime_identity`. The root
//! `tracedecay` module re-exports it so
//! `crate::tracedecay::current_timestamp` keeps resolving on both sides of the
//! split.

/// Returns the current UNIX timestamp in seconds.
pub fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
