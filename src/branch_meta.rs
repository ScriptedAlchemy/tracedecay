//! Compatibility façade for runtime branch metadata.

pub use tracedecay_runtime_core::branch_meta::*;

pub fn update_synced_timestamp(tracedecay_dir: &std::path::Path, branch: &str) {
    tracedecay_runtime_core::branch_meta::update_synced_timestamp_with_lock(
        tracedecay_dir,
        branch,
        crate::branch::acquire_branch_lock_blocking,
    );
}
