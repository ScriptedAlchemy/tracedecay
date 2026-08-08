//! Consolidated in-process test suite for graph, types, display, context,
//! resolution, bench, cloud, annotation-helper, and complexity tests.
//!
//! Merging these formerly separate integration-test binaries into one binary
//! cuts Windows CI link time (each `tests/*.rs` file links separately).

/// Crate-root shim so `crates/tracedecay-code-extraction/src/annotations.rs`
/// (included via `#[path]` in `annotation_helpers_test`) can resolve its
/// `use crate::types::...` imports inside this test crate.
mod types {
    pub use tracedecay::types::*;
}

#[path = "../common/mod.rs"]
mod common;

/// Hermetic profile shard for a fixture project, pinned inside the fixture's
/// own temporary tree.
///
/// A bare `TraceDecay::init` resolves the profile from `TRACEDECAY_DATA_DIR`,
/// and `.cargo/config.toml` points that at the DURABLE, workspace-resident
/// `target/test-profile/.tracedecay`. Pairing a durable profile with a
/// `TempDir` project root is exactly the combination
/// `project_registry::ephemeral_root_rejection` refuses ("project root
/// '/tmp/.tmpXXXX' is under the OS temporary directory and cannot be
/// registered as a durable authority in profile '...'"). The hermetic escape
/// hatch (`TraceDecay::standalone_test_open_options`) is
/// `cfg(test)`/`test-transport` gated, so it is inactive for this integration
/// binary — the fixture must pin the profile itself, the same shape
/// `tests/automation_runner_test::support::fixture_open_options` uses.
///
/// The shard lives under the project's own `.tracedecay/` marker directory so
/// it is ephemeral (satisfying the guard), unique per fixture (no cross-test
/// lifecycle-lease contention), and invisible to the indexer. It is
/// deliberately NOT pre-created: `load_or_create_pinned` only applies the
/// mandatory 0700 restriction to a root it created itself, and a pre-created
/// root would then fail `validate_private_profile_root`.
mod fixture_profile {
    use std::path::Path;

    use tracedecay::tracedecay::TraceDecayOpenOptions;

    pub(crate) fn open_options(project_root: &Path) -> TraceDecayOpenOptions {
        let profile_root = project_root.join(".tracedecay").join("fixture-profile");
        TraceDecayOpenOptions {
            global_db_path: Some(profile_root.join("global.db")),
            profile_root: Some(profile_root),
        }
    }
}

mod annotation_helpers_test;
mod bench_test;
mod cloud_test;
mod complexity_test;
mod context_test;
mod display_test;
mod graph_test;
mod resolution_test;
mod types_test;
