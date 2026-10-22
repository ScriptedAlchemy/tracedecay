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
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;

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
const TEMPORARY_CGROUP_RESOURCES: [&str; 8] = [
    "cpu.max",
    "cpu.stat",
    "cpu.pressure",
    "cpuset.cpus.effective",
    "memory.current",
    "memory.max",
    "memory.events",
    "memory.pressure",
];

struct TemporaryWorkerResources {
    output: fs::File,
    daemon_pid: u32,
    started: Instant,
}

fn record_raw_resource(output: &mut fs::File, path: &Path) -> std::io::Result<String> {
    let result = fs::read_to_string(path);
    writeln!(output, "resource_begin path={}", path.display()).expect("resource artifact");
    match &result {
        Ok(raw) if path.ends_with("status") => {
            for field in [
                "Threads:",
                "Cpus_allowed_list:",
                "Mems_allowed_list:",
                "VmRSS:",
                "VmHWM:",
            ] {
                match raw.lines().find(|line| line.starts_with(field)) {
                    Some(line) => writeln!(output, "{line}"),
                    None => writeln!(output, "unavailable field={field}"),
                }
                .expect("resource artifact");
            }
        }
        Ok(raw) if path == Path::new("/proc/cpuinfo") => {
            for field in ["processor", "vendor_id", "model name", "cpu MHz"] {
                match raw.lines().find(|line| line.starts_with(field)) {
                    Some(line) => writeln!(output, "{line}"),
                    None => writeln!(output, "unavailable field={field}"),
                }
                .expect("resource artifact");
            }
        }
        Ok(raw) if path == Path::new("/proc/stat") => {
            match raw.lines().find(|line| line.starts_with("cpu ")) {
                Some(line) => writeln!(output, "{line}"),
                None => writeln!(output, "unavailable aggregate cpu line"),
            }
            .expect("resource artifact");
        }
        Ok(raw) => writeln!(output, "{raw}").expect("resource artifact"),
        Err(error) => writeln!(output, "unavailable error={error:?}").expect("resource artifact"),
    }
    writeln!(output, "resource_end path={}", path.display()).expect("resource artifact");
    result
}

impl TemporaryWorkerResources {
    fn sample(&mut self, event: &str) {
        let sample_started = Instant::now();
        writeln!(
            self.output,
            "TEMPORARY_DIAGNOSTIC_MUST_NOT_MERGE sample_begin event={event} unix_time={:?} elapsed={:?} test_pid={} daemon_pid={} available_parallelism={:?}",
            SystemTime::now().duration_since(UNIX_EPOCH),
            self.started.elapsed(),
            std::process::id(),
            self.daemon_pid,
            std::thread::available_parallelism(),
        )
        .expect("resource artifact");
        let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clock_ticks > 0 {
            writeln!(
                self.output,
                "proc_stat_clock_ticks_per_second={clock_ticks}"
            )
            .expect("resource artifact");
        } else {
            writeln!(self.output, "proc_stat_clock_ticks_per_second=unavailable sysconf_result={clock_ticks}; proc_stat_cpu_times_are_raw_ticks")
                .expect("resource artifact");
        }
        for path in [
            "/proc/loadavg",
            "/proc/stat",
            "/proc/pressure/cpu",
            "/proc/pressure/io",
            "/proc/pressure/memory",
        ] {
            let _ = record_raw_resource(&mut self.output, Path::new(path));
        }
        let root = Path::new("/sys/fs/cgroup");
        writeln!(
            self.output,
            "cgroup_scope=visible_namespace_root effective_daemon_quota=unproven"
        )
        .expect("resource artifact");
        for name in TEMPORARY_CGROUP_RESOURCES {
            let _ = record_raw_resource(&mut self.output, &root.join(name));
        }
        let threads = PathBuf::from(format!("/proc/{}/task", self.daemon_pid));
        match fs::read_dir(&threads) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            for name in ["comm", "stat", "schedstat"] {
                                let _ =
                                    record_raw_resource(&mut self.output, &entry.path().join(name));
                            }
                        }
                        Err(error) => {
                            writeln!(self.output, "thread_entry_unavailable error={error:?}")
                                .expect("resource artifact")
                        }
                    }
                }
            }
            Err(error) => writeln!(self.output, "threads_unavailable error={error:?}")
                .expect("resource artifact"),
        }
        for pid in [std::process::id(), self.daemon_pid] {
            for name in ["stat", "schedstat", "io", "status"] {
                let _ = record_raw_resource(
                    &mut self.output,
                    &PathBuf::from(format!("/proc/{pid}/{name}")),
                );
            }
            let membership = record_raw_resource(
                &mut self.output,
                &PathBuf::from(format!("/proc/{pid}/cgroup")),
            );
            let Ok(membership) = membership else {
                continue;
            };
            let Some(relative) = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::/"))
            else {
                writeln!(
                    self.output,
                    "cgroup_v2_unavailable pid={pid} reason=no_unified_membership"
                )
                .expect("resource artifact");
                continue;
            };
            if !Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                writeln!(
                    self.output,
                    "cgroup_v2_unavailable pid={pid} reason=membership_outside_visible_root"
                )
                .expect("resource artifact");
                continue;
            }
            let mut directory = root.join(relative);
            for depth in 0..32 {
                if directory == root {
                    break;
                }
                for name in TEMPORARY_CGROUP_RESOURCES {
                    let _ = record_raw_resource(&mut self.output, &directory.join(name));
                }
                if depth == 31 {
                    writeln!(
                        self.output,
                        "cgroup_v2_ancestors_truncated pid={pid} limit=32"
                    )
                    .expect("resource artifact");
                    break;
                }
                directory.pop();
            }
        }
        writeln!(
            self.output,
            "sample_end event={event} elapsed={:?} observation_elapsed={:?}\n",
            self.started.elapsed(),
            sample_started.elapsed()
        )
        .expect("resource artifact");
        self.output.flush().expect("flush resource artifact");
    }
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
    eprintln!("heavy_refresh cli_started args={args:?}");
    let output = tracedecay_command_with_home(home)
        .args(args)
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("tracedecay {args:?}: {error}"));
    eprintln!(
        "heavy_refresh cli_finished args={args:?} elapsed={:?} status={}",
        started.elapsed(),
        output.status
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
fn wait_for_current(
    home: &Path,
    project: &Path,
    revision: &str,
    resources: &mut TemporaryWorkerResources,
) -> String {
    let started = Instant::now();
    let deadline = started + INDEX_TIMEOUT;
    loop {
        resources.sample("status_poll");
        let status = status(home, project);
        eprintln!(
            "heavy_refresh index_status revision={revision} elapsed={:?} status={status}",
            started.elapsed()
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
    eprintln!("heavy_refresh started");
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

    eprintln!(
        "heavy_refresh corpus_written elapsed={:?}",
        started.elapsed()
    );
    let log = PathBuf::from(
        std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR")
            .expect("diagnostic run requires Bazel undeclared outputs"),
    )
    .join("heavy-refresh-daemon.log");
    eprintln!(
        "heavy_refresh daemon_starting elapsed={:?}",
        started.elapsed()
    );
    let daemon = spawn_tracedecay_daemon_logged(&home, &log, |command| {
        command
            .env(
                "TRACEDECAY_RESIDENT_MEMORY_LIMIT_BYTES",
                LIMIT_BYTES.to_string(),
            )
            .env("TRACEDECAY_SPAN_TIMINGS", "1")
            .env(
                "RUST_LOG",
                concat!(
                    "info,",
                    "tracedecay_code_index_runtime::code_index_scheduler::registry=trace,",
                    "tracedecay_code_index_runtime::code_index_scheduler::reconcile=trace,",
                    "tracedecay_code_index_runtime::code_index_scheduler::serving=trace,",
                    "tracedecay_query::retrieval::lexical::projection::artifact::builder=trace,",
                    "tracedecay_query::retrieval::lexical::projection::artifact::prepared=trace,",
                    "tracedecay_code_index::production::lexical_page_source=trace,",
                    "tracedecay_code_index::production::partitioned_codec=trace,",
                    "tracedecay_code_index::production::sealed_codec=trace,",
                    "tracedecay_query::retrieval::lexical::projection::artifact::reader=trace",
                ),
            );
    });
    let mut resources = TemporaryWorkerResources {
        output: fs::File::create(log.with_file_name("heavy-refresh-worker-resources.log"))
            .expect("temporary worker resource artifact"),
        daemon_pid: daemon.id(),
        started,
    };
    writeln!(
        resources.output,
        "cpu_identity_scope=first_visible_processor cpu_mhz_scope=instantaneous"
    )
    .expect("resource artifact");
    let _ = record_raw_resource(&mut resources.output, Path::new("/proc/cpuinfo"));
    resources.sample("daemon_ready");
    eprintln!("heavy_refresh daemon_ready elapsed={:?}", started.elapsed());
    eprintln!("heavy_refresh init_started elapsed={:?}", started.elapsed());
    initialize_tracedecay_cli_project(&home, &project);
    eprintln!(
        "heavy_refresh init_finished elapsed={:?}",
        started.elapsed()
    );
    resources.sample("init_finished");
    let mut generation = wait_for_current(&home, &project, &cold_revision, &mut resources);
    resources.sample("cold_current");
    assert_within_limit(&home, &project, daemon.id());

    for edit in 1..=REFRESHES {
        resources.sample(&format!("edit_{edit}_started"));
        eprintln!(
            "heavy_refresh edit_started edit={edit} elapsed={:?}",
            started.elapsed()
        );
        write_corpus(&project, edit);
        git_run(&project, &["commit", "--quiet", "-am", "edit every file"]);
        let revision = git_capture(&project, &["rev-parse", "HEAD"]);
        resources.sample(&format!("edit_{edit}_sync_started"));
        run_cli(&home, &project, &["sync"]);
        let refreshed = wait_for_current(&home, &project, &revision, &mut resources);
        resources.sample(&format!("edit_{edit}_current"));
        assert_ne!(
            refreshed, generation,
            "refresh {edit} published a generation"
        );
        generation = refreshed;
        assert_within_limit(&home, &project, daemon.id());
        eprintln!(
            "heavy_refresh edit_finished edit={edit} elapsed={:?}",
            started.elapsed()
        );
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
    resources.sample("daemon_stopping");
    drop(daemon);
    eprintln!("heavy_refresh finished elapsed={:?}", started.elapsed());
}
