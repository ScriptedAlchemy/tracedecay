//! Configuration surfaces this crate needs without depending on the root
//! `crate::config` module (another effort is splitting that module).
//!
//! Path primitives and generated-segment classification already live in
//! runtime-core / domain. Semantic and retrieval types live in global-db and
//! usecases. The watcher-only sync knobs are constructor-injected as
//! [`crate::ports::GitWatchSyncConfigV1`].

pub use tracedecay_domain::source_path_policy::is_generated_dir_segment;
#[cfg(test)]
pub use tracedecay_global_db::configuration::{registry, resolver};
pub use tracedecay_runtime_core::config::{TRACEDECAY_DIR, is_ambient_project_root};
pub use tracedecay_usecases::config::retrieval;

/// Path-level generated/vendored check used by the scheduler snapshot filter.
///
/// Mirrors the root helper: a minified-asset suffix or any generated directory
/// segment. Kept here so the scheduler does not import root config.
pub fn is_generated_path_segment(path: &str) -> bool {
    has_minified_suffix(path) || path.split('/').any(is_generated_dir_segment)
}

fn has_minified_suffix(path: &str) -> bool {
    path.rfind(".min.").is_some_and(|idx| idx + 5 < path.len())
}
