//! Live managed-daemon integration suite.
//!
//! Everything here talks to the **real, already-running** TraceDecay daemon
//! owned by the operator's profile. That makes it fundamentally different from
//! the rest of `tests/`, which spins up isolated fixtures, so it is guarded
//! twice:
//!
//! 1. every test is `#[ignore]`, so a plain `cargo test` (or nextest) run
//!    executes zero of them; and
//! 2. every test additionally requires `TRACEDECAY_LIVE_DAEMON_TESTS=1`, so
//!    even `cargo test -- --ignored` stays inert unless an operator opted in.
//!
//! The suite is **strictly read-only** against the daemon and the profile:
//!
//! * the daemon handshake is built with `allow_init = false`, so an unindexed
//!   project is reported as an error instead of being created;
//! * only read tools are dispatched — no `fact_store`, no edit/apply surface,
//!   no `init`, no ingest;
//! * the daemon is never started, stopped, restarted, or signalled — the suite
//!   asserts the daemon it found is still the same process when it finishes;
//! * the only child process it spawns is a short-lived `tracedecay serve`
//!   stdio proxy, which it kills itself.
//!
//! `tracedecay_memory_status` is deliberately **not** in the default battery:
//! its handler calls `memory_status_with_repair_v1()`, which repairs derived
//! holographic vectors and is therefore a write. It is available behind
//! `TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS=1` for operators who accept
//! that repair pass.
//!
//! Operator entry point: `scripts/live-daemon-check.sh`.
//!
//! This suite is deliberately **not** the CI performance harness. It is
//! sequential, it asserts fixed latency ceilings rather than reporting
//! percentiles, and it only ever observes a daemon someone else started.
//! `scripts/perf-gate.sh` is the isolated counterpart: it indexes this repo
//! into a throwaway profile, starts its own daemon, and drives concurrent
//! readers at it. Reach for that one when the question is "did serving get
//! slower", and for this one when the question is "is the operator's daemon
//! healthy right now".
//!
//! Environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `TRACEDECAY_LIVE_DAEMON_TESTS` | must be `1` to run anything |
//! | `TRACEDECAY_LIVE_DAEMON_PROJECT` | project root to route to (default: cwd) |
//! | `TRACEDECAY_LIVE_DAEMON_SYMBOL` | symbol name for the search/callers probe |
//! | `TRACEDECAY_LIVE_DAEMON_PATTERN` | literal pattern for the grep probe |
//! | `TRACEDECAY_BIN` | installed binary to cross-check (default: `tracedecay` on `PATH`) |
//! | `TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS` | opt in to the repairing `memory_status` probe |

// The suite drives a Unix domain socket and a stdio proxy; there is no
// Windows-side equivalent to assert against.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use tracedecay::daemon::{
    DaemonHandshake, call_default_tool, default_socket_path, tool_json_payload,
};
use tracedecay::tracedecay::TraceDecay;

/// Opt-in gate. Every test returns early unless this is exactly `1`.
const LIVE_ENV: &str = "TRACEDECAY_LIVE_DAEMON_TESTS";

/// Upper bound for any single daemon-brokered read call.
const CALL_BUDGET: Duration = Duration::from_secs(30);

/// Tighter bound for `tracedecay_status`, the cheapest health probe and the
/// one agents hit most often.
const STATUS_BUDGET: Duration = Duration::from_secs(5);

/// Upper bound for the `initialize` / `tools/list` exchange over a freshly
/// spawned `tracedecay serve` proxy, which includes process startup.
const PROXY_BUDGET: Duration = Duration::from_secs(60);

/// Upper bound for one `tracedecay doctor` pass.
const DOCTOR_BUDGET: Duration = Duration::from_secs(180);

/// Minimum size of the daemon's advertised tool catalog.
const MIN_TOOL_COUNT: usize = 100;

/// Tools that must always be advertised by a healthy daemon.
const REQUIRED_TOOL_NAMES: &[&str] = &[
    "tracedecay_search",
    "tracedecay_grep",
    "tracedecay_context",
    "tracedecay_callers",
    "tracedecay_status",
    "tracedecay_storage_status",
    "tracedecay_git_status",
    "tracedecay_files",
    "tracedecay_impact",
];

// ── gating ──────────────────────────────────────────────────────────────

fn live_enabled() -> bool {
    std::env::var(LIVE_ENV).as_deref() == Ok("1")
}

/// Returns `true` when the suite may talk to the live daemon, printing why it
/// declined otherwise. Callers `return` on `false`.
#[must_use]
fn live_gate(test_name: &str) -> bool {
    if live_enabled() {
        return true;
    }
    eprintln!("skipping {test_name}: set {LIVE_ENV}=1 to run the live daemon suite");
    false
}

// ── environment ─────────────────────────────────────────────────────────

/// Project root the suite routes daemon requests to.
///
/// Defaults to the process working directory so the operator script can point
/// the suite at whichever checkout is actually indexed, rather than at the
/// crate directory of whatever worktree happened to build the test binary.
fn live_project() -> PathBuf {
    std::env::var_os("TRACEDECAY_LIVE_DAEMON_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().expect("live daemon suite needs a readable working directory")
        })
}

fn live_symbol() -> String {
    std::env::var("TRACEDECAY_LIVE_DAEMON_SYMBOL").unwrap_or_else(|_| "DaemonHandshake".to_string())
}

fn live_pattern() -> String {
    std::env::var("TRACEDECAY_LIVE_DAEMON_PATTERN")
        .unwrap_or_else(|_| "DaemonHandshake".to_string())
}

fn installed_binary() -> PathBuf {
    std::env::var_os("TRACEDECAY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tracedecay"))
}

fn memory_status_allowed() -> bool {
    std::env::var("TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS").as_deref() == Ok("1")
}

/// Read-only handshake for the live project.
///
/// `allow_init = false` is the load-bearing argument: it stops the daemon from
/// creating a store for a project that has never been indexed, which would be
/// a durable mutation.
fn read_only_handshake() -> DaemonHandshake {
    DaemonHandshake::for_current_client(
        Some(live_project()),
        None,  // no scope prefix: probe the whole project
        false, // timings off
        false, // allow_init: never create a store from this suite
    )
    .expect("building a daemon handshake must not fail")
}

// ── low-level probes ────────────────────────────────────────────────────

/// Asserts the daemon socket exists and accepts a connection.
///
/// Nothing is written, so this cannot disturb daemon state; it only proves the
/// listener is bound and accepting.
async fn assert_socket_connectable(context: &str) -> PathBuf {
    let socket = default_socket_path().expect("daemon socket path must resolve");
    assert!(
        socket.exists(),
        "{context}: daemon socket {} does not exist — is the managed daemon running?",
        socket.display()
    );
    let connect = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::UnixStream::connect(&socket),
    )
    .await;
    match connect {
        Ok(Ok(stream)) => drop(stream),
        Ok(Err(error)) => panic!(
            "{context}: could not connect to daemon socket {}: {error}",
            socket.display()
        ),
        Err(_) => panic!(
            "{context}: connecting to daemon socket {} timed out",
            socket.display()
        ),
    }
    socket
}

/// One daemon-brokered read call, with its wall-clock cost.
struct TimedCall {
    tool: &'static str,
    payload: Value,
    elapsed: Duration,
}

/// Dispatches a single read tool through the daemon and decodes its JSON
/// payload, bounding the call at [`CALL_BUDGET`].
async fn read_tool(handshake: &DaemonHandshake, tool: &'static str, arguments: Value) -> TimedCall {
    let started = Instant::now();
    let result = tokio::time::timeout(
        CALL_BUDGET,
        call_default_tool(handshake, tool, arguments.clone()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{tool} did not answer within {}s (arguments: {arguments})",
            CALL_BUDGET.as_secs()
        )
    })
    .unwrap_or_else(|error| panic!("{tool} failed against the live daemon: {error}"));
    let elapsed = started.elapsed();
    let payload = tool_json_payload(&result, tool).unwrap_or_else(|error| {
        panic!("{tool} returned no decodable JSON payload: {error} (raw: {result})")
    });
    TimedCall {
        tool,
        payload,
        elapsed,
    }
}

/// Guards every daemon-facing test: the project must already be indexed, or
/// the read-only handshake has nothing to route to.
fn assert_project_indexed() -> PathBuf {
    let project = live_project();
    assert!(
        TraceDecay::is_initialized(&project),
        "live daemon suite needs an already-indexed project; {} is not initialized. \
         Point TRACEDECAY_LIVE_DAEMON_PROJECT at an indexed checkout — this suite \
         never initializes one itself.",
        project.display()
    );
    project
}

/// Extracts the daemon's own PID from a `tracedecay_status` payload.
///
/// `storage_health.daemon_owner_pid` is stamped by the status handler only
/// when it runs inside the daemon, so a present value is also evidence the
/// call was genuinely daemon-brokered rather than answered in-process.
fn daemon_owner_pid(status: &Value) -> u64 {
    status
        .pointer("/storage_health/daemon_owner_pid")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            panic!("tracedecay_status did not report storage_health.daemon_owner_pid: {status}")
        })
}

/// The first search hit that resolved to a unique graph node.
fn first_resolved_node_id(search: &Value) -> Option<String> {
    search
        .get("results")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|hit| {
            hit.get("node_id")
                .or_else(|| hit.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

// ── the read battery ────────────────────────────────────────────────────

/// Runs the daemon-brokered read battery once and returns every call with its
/// payload and latency.
///
/// Every tool here is read-only. `format: "json"` is passed only to tools whose
/// schemas accept it; the typed application surfaces (`storage_status`) declare
/// closed schemas and are called with their exact arguments.
async fn run_read_battery(handshake: &DaemonHandshake) -> Vec<TimedCall> {
    let mut calls = Vec::new();

    let status = read_tool(handshake, "tracedecay_status", json!({ "format": "json" })).await;
    calls.push(status);

    let search = read_tool(
        handshake,
        "tracedecay_search",
        json!({ "query": live_symbol(), "limit": 10, "format": "json" }),
    )
    .await;
    let node_id = first_resolved_node_id(&search.payload);
    calls.push(search);

    calls.push(
        read_tool(
            handshake,
            "tracedecay_grep",
            json!({
                "pattern": live_pattern(),
                "fixed_strings": true,
                "max_results": 20,
                "format": "json"
            }),
        )
        .await,
    );

    let node_id = node_id.unwrap_or_else(|| {
        panic!(
            "tracedecay_search returned no result with a resolvable node_id for '{}'; \
             set TRACEDECAY_LIVE_DAEMON_SYMBOL to a symbol in the indexed project",
            live_symbol()
        )
    });
    calls.push(
        read_tool(
            handshake,
            "tracedecay_callers",
            json!({ "node_id": node_id, "max_depth": 1, "format": "json" }),
        )
        .await,
    );

    calls.push(read_tool(handshake, "tracedecay_storage_status", json!({})).await);

    calls.push(
        read_tool(
            handshake,
            "tracedecay_git_status",
            json!({ "format": "json" }),
        )
        .await,
    );

    if memory_status_allowed() {
        calls.push(
            read_tool(
                handshake,
                "tracedecay_memory_status",
                json!({ "format": "json" }),
            )
            .await,
        );
    } else {
        eprintln!(
            "note: skipping tracedecay_memory_status (it repairs derived vectors); \
             set TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS=1 to include it"
        );
    }

    calls
}

fn call<'a>(calls: &'a [TimedCall], tool: &str) -> &'a TimedCall {
    calls
        .iter()
        .find(|call| call.tool == tool)
        .unwrap_or_else(|| panic!("{tool} was not part of the read battery"))
}

// ── serve stdio proxy ───────────────────────────────────────────────────

/// A short-lived `tracedecay serve` stdio proxy fronting the live daemon.
///
/// This is the production MCP path (the same one Cursor and Claude Code use),
/// so `initialize` and `tools/list` answered here are the daemon's real
/// negotiated responses rather than a re-derived local catalog.
struct ServeProxy {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    /// Accumulates the child's stderr in the background so a failure can
    /// report *why* the process died instead of just that its stdout closed.
    /// `tracedecay serve` is otherwise silent on success, so nulling stderr
    /// (the prior behavior) discarded the one diagnostic channel available
    /// when it exits early.
    stderr: tokio::task::JoinHandle<String>,
}

impl ServeProxy {
    async fn spawn(project: &std::path::Path) -> Self {
        let mut child = tokio::process::Command::new(installed_binary())
            .arg("serve")
            .arg("--path")
            .arg(project)
            .current_dir(project)
            // `.cargo/config.toml` pins every cargo-launched process (this
            // test binary included) at a workspace-local profile via
            // `TRACEDECAY_DATA_DIR=target/test-profile/.tracedecay`, so that
            // ordinary `cargo test`/`cargo run` never touches the operator's
            // real `~/.tracedecay` or contends with a live daemon. This
            // suite is the deliberate exception: it targets the operator's
            // real, already-running managed daemon, so the spawned `serve`
            // proxy must resolve the real default profile rather than
            // inheriting the sandbox pin — otherwise it looks for a daemon
            // socket under `target/test-profile/...` that nothing is
            // listening on, fails its "is the daemon available" check
            // immediately, and exits before answering the first request
            // (surfacing upstream as a bare stdout EOF).
            .env_remove(tracedecay::config::USER_DATA_DIR_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|error| {
                panic!(
                    "could not spawn `{} serve`: {error}",
                    installed_binary().display()
                )
            });
        let stdin = child.stdin.take().expect("serve stdin pipe");
        let stdout = BufReader::new(child.stdout.take().expect("serve stdout pipe"));
        let mut child_stderr = child.stderr.take().expect("serve stderr pipe");
        let stderr = tokio::spawn(async move {
            let mut buffer = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut child_stderr, &mut buffer).await;
            buffer
        });
        Self {
            child,
            stdin,
            stdout,
            stderr,
        }
    }

    async fn send(&mut self, message: &Value) {
        let line = format!("{message}\n");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("writing to the serve proxy must succeed");
        self.stdin
            .flush()
            .await
            .expect("flushing the serve proxy must succeed");
    }

    /// Snapshots whatever the child has written to stderr so far, bounded by
    /// a short grace period. Used only for failure diagnostics: a still-alive
    /// child normally has nothing on stderr, and a dead one has already
    /// closed the pipe, so this returns promptly either way.
    async fn stderr_snapshot(&mut self) -> String {
        match tokio::time::timeout(Duration::from_millis(500), &mut self.stderr).await {
            Ok(Ok(captured)) => captured,
            Ok(Err(join_error)) => format!("<stderr capture task failed: {join_error}>"),
            Err(_) => "<stderr still streaming; child has not exited>".to_string(),
        }
    }

    /// Reads response lines until one carries the requested id.
    ///
    /// Notifications and unrelated responses are skipped rather than treated as
    /// failures: the daemon may push `notifications/tools/list_changed` between
    /// a request and its answer.
    async fn response(&mut self, id: i64) -> Value {
        let deadline = tokio::time::Instant::now() + PROXY_BUDGET;
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout_at(deadline, self.stdout.read_line(&mut line))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "serve proxy did not answer request {id} within {}s",
                        PROXY_BUDGET.as_secs()
                    )
                })
                .expect("reading from the serve proxy must succeed");
            if read == 0 {
                let stderr = self.stderr_snapshot().await;
                panic!(
                    "serve proxy closed its stdout before answering request {id}; \
                     child stderr:\n{stderr}"
                );
            }
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                if value.get("error").is_some() {
                    let stderr = self.stderr_snapshot().await;
                    panic!(
                        "serve proxy request {id} failed: {value}; child stderr:\n{stderr}"
                    );
                }
                return value;
            }
        }
    }

    /// Performs the MCP `initialize` handshake and returns its result object.
    async fn initialize(&mut self) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "tracedecay-live-daemon-suite",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }))
        .await;
        let response = self.response(1).await;
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("initialize response carried no result: {response}"))
    }

    async fn tools_list(&mut self) -> Vec<String> {
        self.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await;
        let response = self.response(2).await;
        response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("tools/list carried no tool array: {response}"))
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// Kills the proxy this suite spawned. Only ever this child's PID.
    async fn shutdown(mut self) {
        self.stderr.abort();
        drop(self.stdin);
        let _ = self.child.kill().await;
    }
}

/// Version reported by the installed binary's `--version`.
fn installed_binary_version() -> String {
    let binary = installed_binary();
    let output = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|error| panic!("could not run `{} --version`: {error}", binary.display()));
    assert!(
        output.status.success(),
        "`{} --version` exited with {:?}",
        binary.display(),
        output.status.code()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // clap prints "tracedecay <version>".
    stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("could not parse a version out of `--version` output: {stdout}"))
        .to_string()
}

// ── tests ───────────────────────────────────────────────────────────────

/// (1) The daemon socket is bound and the version it advertises over a real
/// MCP `initialize` matches the installed binary that clients invoke.
///
/// A mismatch is the classic stale-daemon failure after `tracedecay upgrade`:
/// the binary on `PATH` moved forward but the service is still serving the old
/// process.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_socket_connects_and_serves_the_installed_version() {
    if !live_gate("live_daemon_socket_connects_and_serves_the_installed_version") {
        return;
    }
    let project = assert_project_indexed();
    assert_socket_connectable("socket handshake").await;

    let expected = installed_binary_version();
    let mut proxy = ServeProxy::spawn(&project).await;
    let initialize = proxy.initialize().await;
    proxy.shutdown().await;

    let served = initialize
        .pointer("/serverInfo/version")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("initialize result carried no serverInfo.version: {initialize}"));
    assert_eq!(
        served, expected,
        "daemon serves version {served} but the installed binary reports {expected} — \
         the running daemon is stale relative to the binary on PATH"
    );
    assert_eq!(
        initialize
            .pointer("/serverInfo/name")
            .and_then(Value::as_str),
        Some("tracedecay"),
        "unexpected serverInfo.name: {initialize}"
    );
}

/// (2) The daemon advertises a full tool catalog, not a degraded subset.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_tools_list_exposes_the_full_catalog() {
    if !live_gate("live_daemon_tools_list_exposes_the_full_catalog") {
        return;
    }
    let project = assert_project_indexed();

    let mut proxy = ServeProxy::spawn(&project).await;
    let _ = proxy.initialize().await;
    let names = proxy.tools_list().await;
    proxy.shutdown().await;

    assert!(
        names.len() > MIN_TOOL_COUNT,
        "daemon advertised only {} tools (expected more than {MIN_TOOL_COUNT}); \
         a truncated catalog usually means catalog discovery degraded",
        names.len()
    );
    for required in REQUIRED_TOOL_NAMES {
        assert!(
            names.iter().any(|name| name == required),
            "daemon catalog is missing {required}; advertised {} tools",
            names.len()
        );
    }
    let mut sorted = names.clone();
    sorted.sort();
    let unique = {
        let mut unique = sorted.clone();
        unique.dedup();
        unique
    };
    assert_eq!(
        sorted.len(),
        unique.len(),
        "daemon catalog advertised duplicate tool names"
    );
}

/// (3) The read battery returns well-formed typed payloads from the daemon.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_read_battery_returns_typed_payloads() {
    if !live_gate("live_daemon_read_battery_returns_typed_payloads") {
        return;
    }
    assert_project_indexed();
    let handshake = read_only_handshake();
    let calls = run_read_battery(&handshake).await;

    let status = &call(&calls, "tracedecay_status").payload;
    for key in ["node_count", "edge_count", "file_count"] {
        assert!(
            status.get(key).and_then(Value::as_u64).is_some(),
            "tracedecay_status payload has no numeric {key}: {status}"
        );
    }
    assert!(
        status.get("node_count").and_then(Value::as_u64) > Some(0),
        "tracedecay_status reports an empty graph: {status}"
    );
    assert!(
        daemon_owner_pid(status) > 0,
        "tracedecay_status reported a zero daemon pid: {status}"
    );

    let search = &call(&calls, "tracedecay_search").payload;
    let results = search
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("tracedecay_search payload has no results array: {search}"));
    assert!(
        !results.is_empty(),
        "tracedecay_search returned no hits for '{}': {search}",
        live_symbol()
    );

    let grep = &call(&calls, "tracedecay_grep").payload;
    assert!(
        grep.get("results").and_then(Value::as_array).is_some(),
        "tracedecay_grep payload has no results array: {grep}"
    );
    for key in ["match_count", "files_scanned"] {
        assert!(
            grep.get(key).and_then(Value::as_u64).is_some(),
            "tracedecay_grep payload has no numeric {key}: {grep}"
        );
    }
    assert!(
        grep.get("match_count").and_then(Value::as_u64) > Some(0),
        "tracedecay_grep found no matches for '{}': {grep}",
        live_pattern()
    );

    let callers = &call(&calls, "tracedecay_callers").payload;
    assert!(
        callers.is_array() || callers.get("callers").is_some(),
        "tracedecay_callers payload is neither an array nor a callers object: {callers}"
    );

    let storage = &call(&calls, "tracedecay_storage_status").payload;
    assert!(
        storage.is_object() && storage.as_object().is_some_and(|map| !map.is_empty()),
        "tracedecay_storage_status returned an empty payload: {storage}"
    );

    let git = &call(&calls, "tracedecay_git_status").payload;
    assert!(
        git.is_object() && git.as_object().is_some_and(|map| !map.is_empty()),
        "tracedecay_git_status returned an empty payload: {git}"
    );

    if memory_status_allowed() {
        let memory = &call(&calls, "tracedecay_memory_status").payload;
        assert_eq!(
            memory.get("status").and_then(Value::as_str),
            Some("ok"),
            "tracedecay_memory_status did not report ok: {memory}"
        );
    }
}

/// (4) Every read stays inside its latency budget.
///
/// Reported as an aggregate so one run names *all* the slow tools rather than
/// stopping at the first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_read_battery_respects_latency_bounds() {
    if !live_gate("live_daemon_read_battery_respects_latency_bounds") {
        return;
    }
    assert_project_indexed();
    let handshake = read_only_handshake();
    let calls = run_read_battery(&handshake).await;

    let mut over_budget = Vec::new();
    for call in &calls {
        let budget = if call.tool == "tracedecay_status" {
            STATUS_BUDGET
        } else {
            CALL_BUDGET
        };
        if call.elapsed > budget {
            over_budget.push(format!(
                "{} took {:?} (budget {:?})",
                call.tool, call.elapsed, budget
            ));
        }
    }
    assert!(
        over_budget.is_empty(),
        "daemon read calls exceeded their latency budgets: {}",
        over_budget.join("; ")
    );
}

/// (5) `tracedecay doctor` completes without issues.
///
/// Doctor exits zero when it counted zero issues, regardless of how many
/// warnings it printed, so a successful exit *is* the "0 or warnings only"
/// contract. The summary line is captured so a failure names the count.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_doctor_reports_no_issues() {
    if !live_gate("live_daemon_doctor_reports_no_issues") {
        return;
    }
    let project = assert_project_indexed();

    let binary = installed_binary();
    let output = tokio::time::timeout(
        DOCTOR_BUDGET,
        tokio::process::Command::new(&binary)
            .arg("doctor")
            .current_dir(&project)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "`{} doctor` did not finish within {}s",
            binary.display(),
            DOCTOR_BUDGET.as_secs()
        )
    })
    .unwrap_or_else(|error| panic!("could not run `{} doctor`: {error}", binary.display()));

    // Doctor writes its report to stderr.
    let report = String::from_utf8_lossy(&output.stderr);
    let summary = report
        .lines()
        .rev()
        .find(|line| line.contains("checks passed") || line.contains("issue(s)"))
        .unwrap_or("<no doctor summary line>")
        .trim()
        .to_string();
    assert!(
        output.status.success(),
        "`{} doctor` exited with {:?}; summary: {summary}",
        binary.display(),
        output.status.code()
    );
}

/// (6) The daemon survived the battery: same process, socket still accepting.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a running managed daemon; set TRACEDECAY_LIVE_DAEMON_TESTS=1"]
async fn live_daemon_stays_healthy_after_read_battery() {
    if !live_gate("live_daemon_stays_healthy_after_read_battery") {
        return;
    }
    assert_project_indexed();
    let handshake = read_only_handshake();

    let before = read_tool(&handshake, "tracedecay_status", json!({ "format": "json" })).await;
    let pid_before = daemon_owner_pid(&before.payload);

    let _ = run_read_battery(&handshake).await;

    assert_socket_connectable("post-battery health").await;
    let after = read_tool(&handshake, "tracedecay_status", json!({ "format": "json" })).await;
    let pid_after = daemon_owner_pid(&after.payload);

    assert_eq!(
        pid_before, pid_after,
        "the daemon restarted during the read battery (pid {pid_before} -> {pid_after}); \
         a read-only battery must never cause a respawn"
    );
    assert!(
        after.elapsed <= STATUS_BUDGET,
        "post-battery tracedecay_status took {:?} (budget {STATUS_BUDGET:?}) — \
         the daemon is degraded after the battery",
        after.elapsed
    );
}
