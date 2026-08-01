//! Text-level dependency-direction ratchet between the MCP and daemon layers.
//!
//! The workspace guard in `compile_isolation` only sees Cargo edges, so it
//! cannot observe module coupling *inside* the root package. The MCP surface
//! and the daemon are being pulled apart into separately reviewable layers, and
//! the remaining `crate::mcp::` / `crate::daemon::` references are the residue
//! of that not-yet-finished split.
//!
//! # Ratchet intent
//!
//! These tests are deliberately **not** a zero-tolerance boundary check: the
//! current edges are recorded, per file, in the allowlists below. The job of
//! this target is narrow and mechanical:
//!
//! * a file that gains *more* cross-layer references than it has today fails;
//! * a file that has *no* recorded budget and gains its first reference fails.
//!
//! Removing references never fails. When a de-knotting change drops the count
//! for a file, lower (or delete) its entry so the ratchet keeps tightening —
//! that edit is the point of the allowlist, not an obstacle to it. The numbers
//! are a debt ledger, and the only legal direction of travel is down.
//!
//! The scan is intentionally a plain substring count over `.rs` sources rather
//! than a syntactic analysis. It has no dependencies beyond `std`, it cannot
//! drift out of sync with the compiler's view in a way that hides an edge (a
//! reference written as `crate::daemon::…` is counted wherever it appears,
//! including inside comments and macros), and it is cheap enough to keep in the
//! default test target.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Root-relative sources that may reference `crate::daemon::`, with the number
/// of references each is currently allowed to contain.
///
/// Lower these as the MCP surface stops reaching into the daemon. Do not raise
/// them: a new or larger edge is the regression this target exists to catch.
const MCP_TO_DAEMON_ALLOWLIST: &[(&str, usize)] = &[
    ("src/mcp/hook_events.rs", 28),
    ("src/mcp/project_route.rs", 1),
    ("src/mcp/scope.rs", 2),
    ("src/mcp/server.rs", 5),
    ("src/mcp/server/connection.rs", 3),
    ("src/mcp/server/construction.rs", 7),
    ("src/mcp/server/freshness_tests.rs", 2),
    ("src/mcp/server/hook_boundary_failure_matrix_tests.rs", 1),
    ("src/mcp/server/hook_branch_writer_tests.rs", 1),
    ("src/mcp/server/host_admission_tests.rs", 1),
    ("src/mcp/server/lifecycle.rs", 2),
    ("src/mcp/server/message_search_cutover_tests.rs", 1),
    ("src/mcp/server/protocol.rs", 1),
    ("src/mcp/server/requests.rs", 1),
    ("src/mcp/server/session_refresh.rs", 2),
    ("src/mcp/server/session_retrieval.rs", 3),
    ("src/mcp/tool_analytics.rs", 1),
    ("src/mcp/tools/handlers/admin_cli.rs", 3),
    ("src/mcp/tools/handlers/edit.rs", 2),
    ("src/mcp/tools/handlers/hook_runtime.rs", 13),
    ("src/mcp/tools/handlers/memory/mod.rs", 1),
    ("src/mcp/tools/handlers/memory/status.rs", 1),
    ("src/mcp/tools/handlers/mod.rs", 4),
];

/// Root-relative sources that may reference `crate::mcp::`, with the number of
/// references each is currently allowed to contain.
///
/// The daemon owns admission and proxying for the MCP transport, so this ledger
/// is the larger of the two. It shrinks the same way: by moving shared contracts
/// out of `crate::mcp` rather than by editing the numbers upward.
const DAEMON_TO_MCP_ALLOWLIST: &[(&str, usize)] = &[
    ("src/daemon.rs", 116),
    ("src/daemon/branch_add.rs", 3),
    ("src/daemon/branch_admin.rs", 4),
    ("src/daemon/code_index_scheduler/queries.rs", 3),
    ("src/daemon/core_admission.rs", 6),
    ("src/daemon/core_doctor.rs", 2),
    ("src/daemon/core_proxy.rs", 6),
    ("src/daemon/hook_v2_replay.rs", 1),
    ("src/daemon/profile_host_admission_replay.rs", 1),
    ("src/daemon/project_open_owners.rs", 3),
    ("src/daemon/query_mcp_admission.rs", 7),
    ("src/daemon/tests.rs", 5),
    ("src/daemon/tests/bootstrap.rs", 3),
    ("src/daemon/tests/code_index_hydration.rs", 1),
    ("src/daemon/tests/ownership.rs", 2),
    ("src/daemon/tests/restart_proxy.rs", 4),
    ("src/daemon/tests/rmcp_route.rs", 4),
    ("src/daemon/tests/scheduler_config.rs", 8),
    ("src/daemon/tests/socket.rs", 1),
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collect every `.rs` file under `relative`, which may be a file or directory.
fn rust_sources(relative: &str, into: &mut Vec<PathBuf>) {
    let absolute = crate_root().join(relative);
    if absolute.is_file() {
        if absolute
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            into.push(absolute);
        }
        return;
    }
    if !absolute.is_dir() {
        return;
    }
    let mut pending = vec![absolute];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
        for entry in entries {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                into.push(path);
            }
        }
    }
}

/// Every top-level entry under `src/` whose name starts with `daemon`.
///
/// Naming the roots by prefix rather than listing them keeps a newly added
/// `src/daemon_*.rs` inside the guard from the moment it lands, instead of
/// silently sitting outside it until someone remembers to extend a list.
fn daemon_roots() -> Vec<String> {
    let mut roots = fs::read_dir(crate_root().join("src"))
        .expect("read src")
        .filter_map(|entry| {
            let name = entry.expect("read src entry").file_name();
            let name = name.to_string_lossy().into_owned();
            name.starts_with("daemon").then(|| format!("src/{name}"))
        })
        .collect::<Vec<_>>();
    roots.sort();
    assert!(
        roots.iter().any(|root| root == "src/daemon.rs"),
        "expected src/daemon.rs to anchor the daemon-side scan"
    );
    roots
}

/// Root-relative, forward-slash path used as the allowlist key.
fn allowlist_key(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Count non-overlapping occurrences of `needle` in every scanned source.
fn reference_counts<R: AsRef<str>>(roots: &[R], needle: &str) -> BTreeMap<String, usize> {
    let mut sources = Vec::new();
    for root in roots {
        rust_sources(root.as_ref(), &mut sources);
    }
    let mut counts = BTreeMap::new();
    for source in sources {
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        let occurrences = contents.matches(needle).count();
        if occurrences > 0 {
            counts.insert(allowlist_key(&source), occurrences);
        }
    }
    counts
}

fn assert_ratchet(
    counts: &BTreeMap<String, usize>,
    allowlist: &[(&str, usize)],
    needle: &str,
    layer: &str,
) {
    let budgets = allowlist.iter().copied().collect::<BTreeMap<&str, usize>>();

    let mut regressions = Vec::new();
    for (path, actual) in counts {
        match budgets.get(path.as_str()) {
            Some(&allowed) if *actual > allowed => regressions.push(format!(
                "{path}: {actual} references to `{needle}` exceeds the recorded budget of {allowed}"
            )),
            Some(_) => {}
            None => regressions.push(format!(
                "{path}: {actual} new references to `{needle}` in a file that had none"
            )),
        }
    }

    assert!(
        regressions.is_empty(),
        "the {layer} boundary regressed; new cross-layer references are not accepted here. \
         Route the shared type or helper through a contract module both layers may depend on \
         instead of widening this ledger:\n  {}",
        regressions.join("\n  ")
    );
}

#[test]
fn mcp_does_not_grow_new_references_into_the_daemon() {
    let counts = reference_counts(&["src/mcp"], "crate::daemon::");
    assert_ratchet(
        &counts,
        MCP_TO_DAEMON_ALLOWLIST,
        "crate::daemon::",
        "MCP-to-daemon",
    );
}

#[test]
fn the_daemon_does_not_grow_new_references_into_mcp() {
    let counts = reference_counts(&daemon_roots(), "crate::mcp::");
    assert_ratchet(
        &counts,
        DAEMON_TO_MCP_ALLOWLIST,
        "crate::mcp::",
        "daemon-to-MCP",
    );
}

/// The ledgers only mean something if they are actually being read: a typo in a
/// path would silently turn a budget into dead text and let that file grow
/// without limit.
#[test]
fn allowlisted_paths_exist() {
    let missing = MCP_TO_DAEMON_ALLOWLIST
        .iter()
        .chain(DAEMON_TO_MCP_ALLOWLIST.iter())
        .map(|(path, _)| *path)
        .filter(|path| !crate_root().join(path).is_file())
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "these allowlist entries no longer name a real source file; delete them \
         (the edge is gone) or fix the path:\n  {}",
        missing.join("\n  ")
    );
}
