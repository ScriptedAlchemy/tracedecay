//! Cargo manifest and source-layout architecture guards.
//!
//! Generic metadata and filesystem inspection live separately from policy and
//! the checked-in frozen workspace snapshot.

mod cargo;
mod fixture;
mod physical;
mod policy;

#[cfg(test)]
mod physical_tests;
#[cfg(test)]
mod tests;

pub(crate) use cargo::{cargo_source_layout, filesystem_rust_sources};
pub(crate) use physical::{
    git_tracked_paths, inspect_physical_manifest_paths, physical_manifest_layout,
};

use crate::module_scanner::resolve_reachable_sources;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// These sample-project sources are intentionally indexed inputs rather than
// modules or targets of the TraceDecay crate.
const INTENTIONAL_STANDALONE_RUST_INPUTS: &[&str] = &[
    "tests/fixtures/context_eval_project/src/auth/login.rs",
    "tests/fixtures/context_eval_project/src/auth/mod.rs",
    "tests/fixtures/context_eval_project/src/auth/session.rs",
    "tests/fixtures/context_eval_project/src/cli.rs",
    "tests/fixtures/context_eval_project/src/main.rs",
    "tests/fixtures/context_eval_project/src/net/http_client.rs",
    "tests/fixtures/context_eval_project/src/net/mod.rs",
    "tests/fixtures/context_eval_project/src/net/retry.rs",
    "tests/fixtures/context_eval_project/src/storage/cache.rs",
    "tests/fixtures/context_eval_project/src/storage/config_store.rs",
    "tests/fixtures/context_eval_project/src/storage/mod.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/id.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs",
];

#[test]
fn git_tracked_rust_sources_are_reachable_from_cargo_targets() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let layout = cargo_source_layout(repository).expect("discover Cargo workspace Rust targets");
    let reachable = resolve_reachable_sources(repository, &layout.target_roots)
        .expect("resolve Rust module/include graph");
    let tracked = cargo::git_tracked_rust_sources(repository, &layout.tracked_roots)
        .expect("list git-tracked workspace Rust sources");
    let allowlisted: BTreeSet<PathBuf> = INTENTIONAL_STANDALONE_RUST_INPUTS
        .iter()
        .map(PathBuf::from)
        .collect();
    let stale_allowlist: Vec<_> = allowlisted.difference(&tracked).collect();
    assert!(
        stale_allowlist.is_empty(),
        "standalone Rust input allowlist contains untracked or deleted paths: {stale_allowlist:?}"
    );
    let reachable_allowlist: Vec<_> = allowlisted.intersection(&reachable).collect();
    assert!(
        reachable_allowlist.is_empty(),
        "Rust inputs are now reachable and should leave the standalone allowlist: {reachable_allowlist:?}"
    );
    let orphaned: Vec<_> = tracked
        .difference(&reachable)
        .filter(|path| !allowlisted.contains(*path))
        .collect();
    assert!(
        orphaned.is_empty(),
        "git-tracked Rust files are not reachable from any Cargo target:\n{}\n\
         Register each file from a target/module root, or document a genuinely standalone source \
         input in INTENTIONAL_STANDALONE_RUST_INPUTS.",
        orphaned
            .iter()
            .map(|path| format!("  - {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
