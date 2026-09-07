//! Hotpath lane coverage for `GitRepositoryAuthority`.
//!
//! One binary, two mutually exclusive configurations, so `--test
//! git_repository_authority_hotpath` means the opposite proof depending on
//! the feature set:
//!
//! - feature-off (default features): the same authority workload runs with
//!   every Hotpath report/output variable set, and must leave no report file
//!   and no live-metrics/MCP listener on 6770/6771. The profiler has to be
//!   compiled out, not merely idle.
//! - `--features hotpath`: one process-boundary guard wraps the workload with
//!   the metrics server off and the CPU section excluded (never autospawn
//!   `samply`; CPU sampling stays opt-in via an explicit `HOTPATH_REPORT`).
//!   The dropped guard's JSON report must carry the static labels of the
//!   production probes this suite pins. The probes themselves live in
//!   `src/git_repository*` and are asserted, never restamped, here.
//!
//! Exactly one `#[test]` exists per configuration: the guard is
//! process-global (at most one may be alive) and both tests mutate process
//! environment. Keep it that way, or run with `--test-threads=1`.
//!
//! ```sh
//! cargo test -p tracedecay-runtime-core --locked \
//!     --test git_repository_authority_hotpath
//! cargo test -p tracedecay-runtime-core --locked --features hotpath \
//!     --test git_repository_authority_hotpath -- --test-threads=1
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_runtime_core::git_repository::{GitHistoryOptions, GitRepositoryAuthority};

// The alloc lane refuses to start without a counting global allocator, and a
// test binary registers its own. Timing-only (`hotpath`) builds keep the
// system allocator.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

/// Fixed localhost ports the Hotpath live metrics and MCP servers would bind.
const HOTPATH_PORTS: [u16; 2] = [6770, 6771];

/// Every static `#[hotpath::measure]` label on the `GitRepositoryAuthority`
/// surface this suite exercises. Compile-time constants on purpose: a label
/// edit in production code must show up here as a deliberate diff, and the
/// feature-on report assertion fails if a probe disappears.
#[cfg(feature = "hotpath")]
const AUTHORITY_LABELS: [&str; 4] = [
    "runtime_core.git.repository_discover",
    "runtime_core.git.references",
    "runtime_core.git.status",
    "runtime_core.git.history",
];

fn assert_no_hotpath_listener() {
    for port in HOTPATH_PORTS {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_err(),
            "unexpected listener on 127.0.0.1:{port}: the Hotpath metrics/MCP \
             server must stay off in this lane"
        );
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git executable");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One commit plus one dirty file, so discover/references/status/history all
/// have real work to do.
fn fixture() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "--quiet", "-b", "main"]);
    std::fs::write(directory.path().join("README.md"), "fixture\n").expect("fixture file");
    git(directory.path(), &["add", "-A"]);
    git(directory.path(), &["commit", "--quiet", "-m", "initial"]);
    std::fs::write(directory.path().join("dirty.txt"), "dirty\n").expect("dirty file");
    directory
}

/// Drives each measured entry point once and asserts the results, so a lane
/// difference in behavior (not just in instrumentation) also fails.
fn run_authority_workload(root: &Path) {
    let authority = GitRepositoryAuthority::discover(root).expect("discover repository");
    assert!(
        authority
            .references()
            .expect("references")
            .iter()
            .any(|reference| reference.name == "refs/heads/main")
    );
    assert_eq!(authority.status().expect("status").entries.len(), 1);
    let history = authority
        .history(&GitHistoryOptions {
            max_count: 10,
            first_parent: false,
            path: None,
            follow_renames: false,
        })
        .expect("history");
    assert_eq!(history.commits.len(), 1);
    assert!(!history.truncated);
}

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use super::{assert_no_hotpath_listener, fixture, run_authority_workload};

    /// The feature-off contract: with every report/output variable pointing
    /// at a writable destination, the workload still produces no report file
    /// and no listener, because no profiler is compiled in to read them.
    #[test]
    fn authority_workload_leaves_no_report_and_no_listener() {
        let report_directory = tempfile::tempdir().expect("report directory");
        let report_path = report_directory.path().join("hotpath-report.json");
        // SAFETY: this configuration compiles exactly one test into the
        // binary and the variables are set before the workload spawns any
        // thread that could read the environment concurrently.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report_path);
            std::env::set_var("HOTPATH_REPORT", "functions-timing");
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "0");
        }

        let repository = fixture();
        run_authority_workload(repository.path());

        assert_no_hotpath_listener();
        assert!(
            !report_path.exists(),
            "feature-off build must not write a Hotpath report"
        );
    }
}

#[cfg(feature = "hotpath")]
mod feature_on {
    use super::{AUTHORITY_LABELS, assert_no_hotpath_listener, fixture, run_authority_workload};

    /// The feature-on contract: one guard, metrics server off, CPU section
    /// excluded, and the exit report lands in the requested file carrying
    /// the static labels of the production probes.
    #[test]
    fn guard_reports_authority_labels_without_metrics_server() {
        let report_directory = tempfile::tempdir().expect("report directory");
        let report_path = report_directory.path().join("hotpath-report.json");
        // Environment overrides builder configuration, so pin it: the
        // metrics server stays off (losing a fixed-port race would print a
        // Hotpath error onto this process's stderr), and no ambient output
        // variables may redirect the report this test asserts on.
        // SAFETY: single test in this configuration; set before the guard or
        // workload spawn any thread reading the environment.
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
            std::env::remove_var("HOTPATH_OUTPUT_FORMAT");
            std::env::remove_var("HOTPATH_OUTPUT_PATH");
            std::env::remove_var("HOTPATH_REPORT");
        }

        let repository = fixture();
        {
            // Mirrors the CLI's process-boundary guard: the CPU section
            // autospawns an external sampler that SIGSTOPs the process while
            // attaching, so it stays excluded by default everywhere and is
            // opt-in via an explicit HOTPATH_REPORT only.
            let _guard = hotpath::HotpathGuardBuilder::new("git-repository-authority-tests")
                .sections_exclude(vec![hotpath::Section::FunctionsCpu])
                .format(hotpath::Format::Json)
                .output_path(&report_path)
                .build();
            run_authority_workload(repository.path());
            // Guard alive and collecting; the server-off switch must hold.
            assert_no_hotpath_listener();
        }

        let report = std::fs::read_to_string(&report_path)
            .expect("guard drop must write the JSON report to the requested path");
        for label in AUTHORITY_LABELS {
            assert!(
                report.contains(label),
                "hotpath report is missing static probe label {label:?}; \
                 report was: {report}"
            );
        }
    }
}
