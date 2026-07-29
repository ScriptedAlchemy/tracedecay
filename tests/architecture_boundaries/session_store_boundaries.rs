//! Guards for the session <-> registered-database boundary.
//!
//! `src/sessions` owns session/transcript/LCM domain logic and declares narrow
//! store ports; the concrete `RegisteredGlobalDb` authority lives in
//! `src/store` adapters. These guards pin the modules that finished that
//! inversion so a convenience wrapper cannot quietly reintroduce the edge.
//!
//! Both guards name explicit files rather than directory roots, because the
//! sibling `tests.rs` modules legitimately open a registered database to build
//! fixtures. Files are listed directly instead of resolved through
//! `filesystem_rust_sources`, which walks directories only and would drop a
//! file root silently.
//!
//! The forbidden-path scan mirrors the helper in `dependency_boundaries`; it is
//! restated here so this module owns its own contract, and the two can fold
//! together once both stop moving.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::module_scanner::tokenize;
use crate::query_kernel::{scan_extern_crate_bindings, scan_qualified_paths, scan_use_bindings};

/// Session modules that reach persistence only through their own ports, with
/// the registered database supplied by a `crate::store` adapter.
///
/// `crate::store` itself is not forbidden here: `workflow_ingest` takes the
/// concrete sink adapter on purpose so spawn paths keep inherent `Send`
/// futures instead of HRTB trait RPITIT.
const PORT_ONLY_SESSION_MODULES: &[&str] = &[
    "src/sessions/git_correlation.rs",
    "src/sessions/git_correlation/backfill.rs",
    "src/sessions/git_correlation/store.rs",
    "src/sessions/workflow_index.rs",
    "src/sessions/workflow_index/port.rs",
    "src/sessions/workflow_ingest.rs",
    "src/sessions/workflow_state.rs",
];

/// Registered-database modules on the temporal read path that carry no edge
/// back into `crate::sessions`.
const SESSION_FREE_GLOBAL_DB_MODULES: &[&str] = &["src/global_db/session_temporal/mod.rs"];

fn forbidden_prefix(path: &[String], prefixes: &[&[&str]]) -> Option<String> {
    prefixes
        .iter()
        .find(|prefix| {
            path.len() >= prefix.len()
                && path
                    .iter()
                    .zip(prefix.iter())
                    .all(|(segment, expected)| segment == *expected)
        })
        .map(|prefix| prefix.join("::"))
}

/// Reads each guarded module, asserting it still exists, and reports every
/// forbidden import, `extern crate`, or inline qualified path it references.
fn violations_for(modules: &[&str], prefixes: &[&[&str]]) -> BTreeSet<String> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = modules.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
    for path in &sources {
        assert!(
            repository.join(path).is_file(),
            "guarded module {} no longer exists; re-scope this guard instead of letting a \
             renamed or deleted file silently drop out of it",
            path.display()
        );
    }

    let mut violations = BTreeSet::new();
    for path in &sources {
        let source = fs::read_to_string(repository.join(path))
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        let tokens = tokenize(&source);
        let bindings = scan_use_bindings(&tokens)
            .into_iter()
            .chain(scan_extern_crate_bindings(&tokens))
            .map(|binding| binding.path)
            .chain(
                scan_qualified_paths(&tokens)
                    .into_iter()
                    .map(|(_, qualified)| qualified),
            );
        for candidate in bindings {
            if let Some(forbidden) = forbidden_prefix(&candidate, prefixes) {
                violations.insert(format!("{}: references {forbidden}", path.display()));
            }
        }
    }
    violations
}

#[test]
fn inverted_session_modules_do_not_name_the_registered_database() {
    let forbidden: &[&[&str]] = &[
        &["crate", "global_db"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
    ];
    let violations = violations_for(PORT_ONLY_SESSION_MODULES, forbidden);
    assert!(
        violations.is_empty(),
        "these session modules must take store ports, not the registered database or a driver; \
         put the registered-database entry point on the `crate::store` adapter instead:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn session_temporal_read_path_does_not_depend_on_the_sessions_module() {
    let forbidden: &[&[&str]] = &[&["crate", "sessions"]];
    let violations = violations_for(SESSION_FREE_GLOBAL_DB_MODULES, forbidden);
    assert!(
        violations.is_empty(),
        "the temporal read path must name shared session DTOs at their owning crate \
         (`tracedecay_store`), not through the `crate::sessions` re-export:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}
