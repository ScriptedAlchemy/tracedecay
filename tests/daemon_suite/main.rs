//! Consolidated daemon test suite.
//!
//! Covers the git-metadata watcher (design D3), the backstop scheduler (D5),
//! the concurrency governor, and branch-store GC (D6) — the freshness path the
//! daemon drives when git operations happen outside a hooked session.
//!
//! The `GitWatcher` type itself is a crate-private daemon component, so these
//! integration tests validate the *composed behavior* through the same public
//! APIs the watcher orchestrates (`TraceDecay::sync*`, `stale_files_since_commit`,
//! `add_branch_tracking_with_options`, `branch::gc_dead_branch_stores`) against
//! real temp git repos. Watcher-internal wiring (debounce coalescing, event
//! classification, heartbeat staleness) is unit-tested inline in
//! `src/daemon/git_watch.rs`.

#[path = "../common/mod.rs"]
mod common;

mod fixture_authority_test;
#[cfg(feature = "test-transport")]
mod git_watch_test;
#[cfg(unix)]
mod pr_autotrack_test;
mod workflow_handoff_test;
