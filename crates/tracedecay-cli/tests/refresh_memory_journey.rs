//! Under a 6 GiB resident-memory limit, the shipped daemon cold-indexes and
//! repeatedly refreshes a heavy corpus without refusing work it has room for
//! and without its resident set crossing the limit.
//!
//! Its own binary: one daemon indexing a large corpus is the whole test.

#![cfg(all(target_os = "linux", not(feature = "alloc-jemalloc")))]

#[path = "../../tracedecay/tests/common/mod.rs"]
mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_runtime_core::logging::log_daemon_event;

use common::fixture::{git_capture, git_run};
use common::{
    canonical_existing_path, initialize_tracedecay_cli_project, spawn_tracedecay_daemon_logged,
    tool_json_structured_content, tracedecay_command_with_home,
};

const MIB: u64 = 1024 * 1024;
const LIMIT_BYTES: u64 = 6 * 1024 * MIB;
const REFRESHES: usize = 4;
const FILES: usize = 300;
const FUNCTIONS_PER_FILE: usize = 160;
const INDEX_TIMEOUT: Duration = Duration::from_secs(900);
const STATUS_POLL: Duration = Duration::from_secs(5);

fn report_phase(started: Instant, phase: &str, edit: usize) {
    log_daemon_event(
        "refresh_memory_journey_phase",
        &[
            ("phase", phase.to_owned()),
            ("edit", edit.to_string()),
            ("elapsed_ms", started.elapsed().as_millis().to_string()),
        ],
    );
}

/// Every function of every file changes with `edit`, so each refresh
/// reparses the whole corpus.
fn write_corpus(project: &Path, edit: usize) {
    let src = project.join("src");
    fs::create_dir_all(&src).expect("source directory");
    for file in 0..FILES {
        let mut source = String::new();
        for function in 0..FUNCTIONS_PER_FILE {
            writeln!(
                source,
                "pub fn heavy_{file:03}_{function:03}(a: u32, b: u32) -> u32 {{\n    \
                 let c = a.wrapping_mul({function}) + b + {edit};\n    \
                 if c > {file} {{ c - a }} else {{ b.wrapping_add(c) }}\n}}"
            )
            .expect("format source");
        }
        fs::write(src.join(format!("module_{file:03}.rs")), source).expect("write source");
    }
}

fn run_cli(home: &Path, project: &Path, args: &[&str]) -> Vec<u8> {
    let started = Instant::now();
    log_daemon_event(
        "refresh_memory_journey_command_started",
        &[("args", format!("{args:?}"))],
    );
    let output = tracedecay_command_with_home(home)
        .args(args)
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("tracedecay {args:?}: {error}"));
    log_daemon_event(
        "refresh_memory_journey_command_finished",
        &[
            ("args", format!("{args:?}")),
            ("elapsed_ms", started.elapsed().as_millis().to_string()),
            ("status", output.status.to_string()),
        ],
    );
    assert!(
        output.status.success(),
        "tracedecay {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn tool(home: &Path, project: &Path, name: &str, arguments: &Value) -> Value {
    let project_arg = project.to_string_lossy().into_owned();
    let stdout = run_cli(
        home,
        project,
        &[
            "tool",
            "--project",
            &project_arg,
            name,
            "--json",
            "--args",
            &arguments.to_string(),
        ],
    );
    tool_json_structured_content(&stdout)
}

fn status(home: &Path, project: &Path) -> Value {
    tool(
        home,
        project,
        "status",
        &json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        }),
    )
}

/// Waits until the index serves a complete generation of `revision` and
/// returns that generation's id.
fn wait_for_current(home: &Path, project: &Path, revision: &str) -> String {
    let started = Instant::now();
    let deadline = started + INDEX_TIMEOUT;
    loop {
        let status = status(home, project);
        log_daemon_event(
            "refresh_memory_journey_status",
            &[
                ("expected_revision", revision.to_owned()),
                ("elapsed_ms", started.elapsed().as_millis().to_string()),
                ("freshness", status["code_index_freshness"].to_string()),
                ("memory", status["memory"].to_string()),
            ],
        );
        let worktree = &status["code_index_freshness"]["worktree"];
        if status["code_index_freshness"]["status"] == "current"
            && worktree["source_revision"] == revision
        {
            assert_eq!(worktree["coverage"], "complete", "{status}");
            return worktree["latest_generation_id"]
                .as_str()
                .unwrap_or_else(|| panic!("a current index names its generation: {status}"))
                .to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the index never served {revision}: {status}"
        );
        std::thread::sleep(STATUS_POLL);
    }
}

fn process_status_bytes(pid: u32, field: &str) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("daemon /proc status");
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{field} missing from /proc/{pid}/status"));
    kib * 1024
}

/// The daemon's own measurement stays nominal and inside the limit, and its
/// peak resident set never crossed it.
fn assert_within_limit(home: &Path, project: &Path, pid: u32) {
    let status = status(home, project);
    let memory = &status["memory"];
    assert_eq!(memory["limit_bytes"], LIMIT_BYTES, "{memory}");
    assert_eq!(memory["status"], "nominal", "{memory}");
    let resident = memory["resident_bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("the daemon measures itself: {memory}"));
    assert!(resident < LIMIT_BYTES, "{memory}");
    let peak = process_status_bytes(pid, "VmHWM:");
    assert!(
        peak < LIMIT_BYTES,
        "the daemon's peak resident set {peak} B crossed the {LIMIT_BYTES} B limit"
    );
}

#[test]
fn repeated_heavy_refreshes_under_the_limit_are_admitted() {
    let started = Instant::now();
    report_phase(started, "prepare_corpus", 0);
    let home = TempDir::new().expect("isolated home");
    let home = canonical_existing_path(home.path());
    let workspace = TempDir::new().expect("project root");
    let project = canonical_existing_path(workspace.path()).join("heavy");
    fs::create_dir_all(&project).expect("project directory");
    write_corpus(&project, 0);
    git_run(&project, &["init", "--quiet", "-b", "main"]);
    git_run(&project, &["add", "."]);
    git_run(&project, &["commit", "--quiet", "-m", "heavy corpus"]);
    let cold_revision = git_capture(&project, &["rev-parse", "HEAD"]);

    let log = home.join("daemon.log");
    report_phase(started, "start_daemon", 0);
    let daemon = spawn_tracedecay_daemon_logged(&home, &log, |command| {
        command
            .env(
                "TRACEDECAY_RESIDENT_MEMORY_LIMIT_BYTES",
                LIMIT_BYTES.to_string(),
            )
            .env("TRACEDECAY_TEST_DAEMON_LOG_STDERR", "1")
            .env("TRACEDECAY_SPAN_TIMINGS", "1")
            .env(
                "RUST_LOG",
                concat!(
                    "info,",
                    "tracedecay_code_index_runtime::code_index_scheduler::reconcile=trace,",
                    "tracedecay_code_index_runtime::code_index_scheduler::registry=trace,",
                    // Keep registry submodules at info: only its phase-level spans
                    // should be timed, not per-file/page or serving-read work.
                    "tracedecay_code_index_runtime::code_index_scheduler::registry::=info,",
                    "tracedecay::daemon::project_open_admission=trace",
                ),
            );
    });
    report_phase(started, "initialize_project", 0);
    initialize_tracedecay_cli_project(&home, &project);
    report_phase(started, "wait_for_current", 0);
    let mut generation = wait_for_current(&home, &project, &cold_revision);
    assert_within_limit(&home, &project, daemon.id());
    report_phase(started, "generation_current", 0);

    for edit in 1..=REFRESHES {
        report_phase(started, "prepare_corpus", edit);
        write_corpus(&project, edit);
        git_run(&project, &["commit", "--quiet", "-am", "edit every file"]);
        let revision = git_capture(&project, &["rev-parse", "HEAD"]);
        report_phase(started, "sync", edit);
        run_cli(&home, &project, &["sync"]);
        report_phase(started, "wait_for_current", edit);
        let refreshed = wait_for_current(&home, &project, &revision);
        assert_ne!(
            refreshed, generation,
            "refresh {edit} published a generation"
        );
        generation = refreshed;
        assert_within_limit(&home, &project, daemon.id());
        report_phase(started, "generation_current", edit);
    }

    let log = fs::read_to_string(&log).expect("daemon log");
    assert_eq!(
        log.matches("code_index_generation_published").count(),
        1 + REFRESHES,
        "the log records every index the daemon published:\n{log}"
    );
    assert!(
        !log.contains("resident-memory admission")
            && !log.contains("code_index_text_projection_waiting_for_memory"),
        "the daemon refused work it had room for:\n{log}"
    );
    drop(daemon);
    report_phase(started, "complete", REFRESHES);
}
