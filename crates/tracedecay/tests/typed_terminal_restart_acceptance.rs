//! Drives genuinely-induced `PartialEffect` and `ResetRequired` typed
//! terminals through the real CLI transport against a *physically* spawned
//! `tracedecay daemon run` process, then proves each terminal's contract
//! survives a physical kill-and-respawn of that daemon.
//!
//! Both terminals are induced through real production mechanisms, not
//! synthetic envelope fabrication:
//!
//! - `PartialEffect`: the CLI's `TRACEDECAY_TOOL_DEADLINE_MS` control
//!   (`src/tool_command.rs`, `tool_command_deadline`) sets the caller-visible
//!   request deadline, which travels to the daemon on the `tools/call` wire
//!   (`src/mcp/tool_call_deadline.rs`) and is what daemon admission and
//!   settlement measure. A real `fact_store add` runs, commits durably, and is
//!   parked at the commit boundary by the `test-transport` fact-commit barrier
//!   (`crates/tracedecay-runtime-core/src/store/memory/commit_barrier.rs`)
//!   until that deadline has elapsed. The retained memory owner then observes
//!   exactly what production observes when a commit outlives its budget —
//!   commit started, deadline elapsed — and reports `PartialEffect` with a real
//!   committed receipt and a `Reconcile`-only legal action
//!   (`src/daemon/retained_owner/receipts.rs::complete_at`,
//!   `src/daemon/retained_owner/memory.rs::execute_add_on_db`).
//!
//!   The barrier exists because that window is otherwise one fsync wide: a
//!   sweep against a live daemon put the whole commit-to-settlement span under
//!   a tenth of a second inside a ~2.3s request, so a purely wall-clock
//!   induction is a race, not a test. The barrier changes *when* the commit
//!   settles, never *what* the daemon decides.
//!
//! - `ResetRequired`: a second project's already-admitted graph/memory store
//!   is tampered with an extra, unexpected SQLite table before the daemon
//!   ever opens it. `ensure_schema_current_connection` ->
//!   `verify_final_schema_connection` -> `require_exact_final_shape`
//!   (`crates/tracedecay-runtime-core/src/db/migrations.rs`,
//!   `crates/tracedecay-runtime-core/src/db/migrations/final_shape.rs`)
//!   refuses any store whose relational shape is not byte-for-byte the exact
//!   final shape this binary creates, returning `ResetRequired` with a
//!   `Reset`-only legal action for every subsequent open of that store.
mod common;
/// The HTTP, MCP-host, and Rust SDK legs of this same journey.
///
/// This file is a crate root, so a bare `mod` would resolve to
/// `tests/transport_boundaries.rs` — and a file there would be
/// auto-discovered as its own test crate. The `#[path]` keeps the submodule
/// inside this journey's directory.
#[path = "typed_terminal_restart_acceptance/transport_boundaries.rs"]
mod transport_boundaries;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use common::{
    canonical_existing_path, open_test_database, spawn_tracedecay_daemon_with,
    tracedecay_command_with_home,
};

const TOOL_DEADLINE_ENV: &str = "TRACEDECAY_TOOL_DEADLINE_MS";
const FACT_COMMIT_BARRIER_DIR_ENV: &str = "TRACEDECAY_TEST_FACT_COMMIT_BARRIER_DIR";
const MAX_MARKER_TOKEN_BYTES: usize = 36;
/// Request budget for the parked add. It must comfortably outlive a cold
/// dispatch so the worker reaches the commit boundary while the budget is still
/// live; the barrier — not this number — decides when settlement happens.
const PARTIAL_EFFECT_DEADLINE: Duration = Duration::from_secs(8);
const BARRIER_ARRIVAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Fails loudly if a marker could be refused by memory hygiene instead of
/// committing, so a future marker edit cannot silently invalidate the
/// barrier-backed partial-effect journey.
fn assert_marker_is_storable(marker: &str) {
    for token in marker.split_whitespace() {
        assert!(
            token.len() < MAX_MARKER_TOKEN_BYTES,
            "marker token '{token}' is {} bytes; memory hygiene may refuse it as \\
             a high-entropy secret instead of committing the fact this journey parks",
            token.len()
        );
    }
}

fn initialize_project(home: &Path, project: &Path, marker: &str) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        format!("pub fn probe() -> &'static str {{ \"{marker}\" }}\n"),
    )
    .expect("project source file");
    let git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("initialize fixture git repository");
    assert!(git.status.success(), "git init failed: {git:?}");
    crate::common::initialize_tracedecay_cli_project(home, project);
}

/// Tampers an already-initialized project's unified graph/memory SQLite store
/// with an extra, unexpected table, so every later open sees a shape that is
/// not the exact final shape this binary creates.
fn make_store_reset_required(home: &Path, project: &Path) {
    let data_root = tracedecay::storage::profile_sharded_data_root(
        &home.join(".tracedecay"),
        &tracedecay::storage::default_profile_project_id(project),
    );
    let db_path = data_root.join(tracedecay::config::db_filename(&data_root));
    assert!(
        db_path.is_file(),
        "tracedecay init should have created the project store at {}",
        db_path.display()
    );
    common::create_runtime().block_on(async {
        let (db, _) = open_test_database(&db_path)
            .await
            .expect("open the already-initialized project store");
        db.execute_write_batch(
            "typed-terminal-restart-acceptance: seed an unexpected table",
            "CREATE TABLE typed_terminal_reset_required_probe (value INTEGER NOT NULL);",
        )
        .await
        .expect("seed unexpected table fixture");
    });
}

fn tool_command(home: &Path, project: &Path, tool: &str, args: &Value) -> std::process::Command {
    let project_arg = project.to_string_lossy().into_owned();
    let mut command = tracedecay_command_with_home(home);
    command.current_dir(project).args([
        "tool",
        "--project",
        project_arg.as_str(),
        tool,
        "--args",
        args.to_string().as_str(),
        "--json",
    ]);
    command.stdin(Stdio::null());
    command
}

fn parse_tool_output(tool: &str, output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "tool call '{tool}' did not return a JSON envelope on stdout ({error})\n\
             exit: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Runs `tracedecay tool --project <project> <tool> --args <args> --json`
/// against the (already-running) daemon and returns the parsed JSON envelope.
fn tool_call(home: &Path, project: &Path, tool: &str, args: &Value) -> Value {
    let output = tool_command(home, project, tool, args)
        .output()
        .unwrap_or_else(|error| panic!("tool call '{tool}' failed to run: {error}"));
    parse_tool_output(tool, &output)
}

/// The typed application envelope a tool result carries.
///
/// Compatibility-owned tools answer with an MCP tool result whose single text
/// block is the rendered envelope; asking for `format: "json"` makes that block
/// the envelope itself.
fn typed_envelope(result: &Value) -> Value {
    result["content"][0]["text"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| result.clone())
}

/// Arms the daemon's fact-commit barrier, runs one real `fact_store add` whose
/// request deadline expires while its committed transaction is parked there,
/// then releases it and returns the envelope the CLI printed.
fn add_fact_settling_after_its_deadline(
    home: &Path,
    project: &Path,
    barrier_dir: &Path,
    content: &str,
) -> Value {
    std::fs::write(barrier_dir.join("armed"), b"armed\n").expect("arm the fact commit barrier");

    let mut command = tool_command(
        home,
        project,
        "tracedecay_fact_store_add",
        &json!({ "content": content, "category": "general" }),
    );
    command
        .env(
            TOOL_DEADLINE_ENV,
            PARTIAL_EFFECT_DEADLINE.as_millis().to_string(),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let sent_at = Instant::now();
    let mut child = command.spawn().expect("spawn the parked fact_store add");

    let arrived = barrier_dir.join("arrived");
    while !matches!(arrived.try_exists(), Ok(true)) {
        assert!(
            child
                .try_wait()
                .expect("inspect the parked fact_store add")
                .is_none(),
            "the fact_store add settled without ever reaching the durable commit boundary"
        );
        assert!(
            sent_at.elapsed() < BARRIER_ARRIVAL_TIMEOUT,
            "the fact_store add never reached the durable commit boundary within {BARRIER_ARRIVAL_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child
            .try_wait()
            .expect("inspect the parked fact_store add")
            .is_none(),
        "the fact_store add settled before the durable commit boundary could be attributed to it"
    );
    let arrived_at = Instant::now();

    // The effect is durable now and the operation is not settled. Hold it here
    // until the caller's own request deadline has certainly expired, so the
    // owner settles a committed effect against an elapsed budget.
    //
    // The hold is measured from *arrival*, not from spawn: the CLI starts its
    // deadline clock inside its own process, some unknown startup after this
    // test spawned it, so `spawn + deadline` can still be earlier than the real
    // expiry. Arrival is strictly after that clock started, so holding a full
    // deadline plus a margin beyond arrival always outlives it.
    let release_at = arrived_at + PARTIAL_EFFECT_DEADLINE + Duration::from_secs(1);
    while Instant::now() < release_at {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(barrier_dir.join("release"), b"release\n")
        .expect("release the fact commit barrier");

    let output = child
        .wait_with_output()
        .expect("collect the parked fact_store add");
    parse_tool_output("tracedecay_fact_store_add", &output)
}

fn assert_partial_effect_committed_receipt(payload: &Value, context: &str) {
    let problem = &payload["problem"];
    assert_eq!(
        problem["kind"], "partial_effect",
        "{context}: expected a partial_effect problem, got {payload}"
    );
    let legal_actions = problem["legal_actions"].as_array().unwrap_or_else(|| {
        panic!("{context}: partial_effect problem omitted legal_actions: {payload}")
    });
    assert_eq!(
        legal_actions,
        &vec![Value::String("reconcile".to_owned())],
        "{context}: PartialEffect must carry Never retry with only the Reconcile legal action"
    );
    assert_eq!(
        problem["retry"], "never",
        "{context}: PartialEffect must never be retried: {payload}"
    );
    let receipt = &problem["committed_receipt"];
    assert!(
        receipt.is_object(),
        "{context}: partial_effect problem omitted its committed_receipt: {payload}"
    );
    assert_eq!(
        receipt["outcome"], "partial",
        "{context}: committed_receipt must record a partial effect outcome: {payload}"
    );
}

fn assert_reset_required(payload: &Value, context: &str) {
    let problem = &payload["problem"];
    assert_eq!(
        problem["kind"], "reset_required",
        "{context}: expected a reset_required problem, got {payload}"
    );
    let legal_actions = problem["legal_actions"].as_array().unwrap_or_else(|| {
        panic!("{context}: reset_required problem omitted legal_actions: {payload}")
    });
    assert_eq!(
        legal_actions,
        &vec![Value::String("reset".to_owned())],
        "{context}: ResetRequired must carry Never retry with only the Reset legal action"
    );
}

fn spawn_daemon_with_commit_barrier(home: &Path, barrier_dir: &PathBuf) -> common::DaemonProcess {
    let barrier_dir = barrier_dir.clone();
    spawn_tracedecay_daemon_with(home, move |command| {
        command.env(FACT_COMMIT_BARRIER_DIR_ENV, &barrier_dir);
    })
}

#[test]
fn partial_effect_survives_physical_daemon_restart_via_cli() {
    const PARTIAL_EFFECT_MARKER: &str = "cli-partial-9f3c2a";

    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let partial_project = tempfile::TempDir::new().expect("partial effect project");
    let partial_project_path = canonical_existing_path(partial_project.path());
    let barrier = tempfile::TempDir::new().expect("fact commit barrier");
    let barrier_path = canonical_existing_path(barrier.path());

    // Project initialization is daemon-owned (`tracedecay init` requires the
    // daemon-owned code-index scheduler), so the daemon must be up first. The
    // barrier is not armed yet, so initialization commits freely.
    let mut daemon = spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    initialize_project(&home_path, &partial_project_path, "partial-effect-fixture");
    assert_marker_is_storable(PARTIAL_EFFECT_MARKER);

    // Induce a genuine, production PartialEffect: the fact commits durably and
    // the request's own deadline expires before the effect settles, so the
    // owner reports Never + [Reconcile] over a real committed receipt.
    let partial_payload = add_fact_settling_after_its_deadline(
        &home_path,
        &partial_project_path,
        &barrier_path,
        PARTIAL_EFFECT_MARKER,
    );
    assert_partial_effect_committed_receipt(&partial_payload, "first request, pre-restart");

    // Physically stop and reap this daemon process, then start a brand new
    // physical process bound to the same profile. Neither project's on-disk
    // state changes; only the serving process is replaced.
    let first_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the first physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    let mut daemon = spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        first_pid,
        "restart reused the physical daemon process"
    );

    // The partially-committed fact must still be durably present: the
    // committed part of the committed_receipt survived the physical restart.
    let search_result = tool_call(
        &home_path,
        &partial_project_path,
        "tracedecay_fact_store_search",
        &json!({ "query": PARTIAL_EFFECT_MARKER, "format": "json" }),
    );
    assert!(
        search_result["problem"].is_null(),
        "post-restart fact search must not itself fail: {search_result}"
    );
    let search_payload = typed_envelope(&search_result);
    let hits = search_payload["outcome"]["value"]["payload"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("post-restart search omitted a hits array: {search_payload}"));
    assert!(
        hits.iter()
            .any(|hit| hit["fact"]["content"].as_str() == Some(PARTIAL_EFFECT_MARKER)),
        "the partially-committed fact from before the physical restart must survive it: {search_payload}"
    );

    let _ = daemon.kill_and_wait();
}

/// The `ResetRequired` half of the same journey.
///
/// A refused store's project open settles with the typed reset terminal:
/// `wait_for_project_open_publication`
/// (`src/daemon/project_open_orchestration.rs`) publishes the failed open
/// instead of reporting warming forever, and `project_open_problem`
/// (`src/daemon/invocation_dispatch.rs`) maps it to the `reset`-only legal
/// action for every operation. This journey proves that settlement — and that
/// a physical restart repeats it identically, because a restart is not a
/// reset.
#[test]
fn reset_required_survives_physical_daemon_restart_via_cli() {
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = canonical_existing_path(home.path());
    let reset_project = tempfile::TempDir::new().expect("reset required project");
    let reset_project_path = canonical_existing_path(reset_project.path());
    let barrier = tempfile::TempDir::new().expect("fact commit barrier");
    let barrier_path = canonical_existing_path(barrier.path());

    let mut daemon = spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    initialize_project(&home_path, &reset_project_path, "reset-required-fixture");

    // Tamper the store. `tracedecay init` is daemon-owned, so this daemon
    // already opened the project once and holds a verified handle; the
    // incompatible shape is what the *next* process-level open observes, which
    // is exactly the physical restart below.
    make_store_reset_required(&home_path, &reset_project_path);

    let first_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the first physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    let mut daemon = spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        first_pid,
        "restart reused the physical daemon process"
    );

    // Induce a genuine, production ResetRequired: this fresh process opens the
    // tampered store for the first time and refuses the incompatible shape
    // before interpreting it.
    let reset_payload = tool_call(
        &home_path,
        &reset_project_path,
        "tracedecay_storage_status",
        &json!({ "include_details": false }),
    );
    assert_reset_required(&reset_payload, "first observation");

    // Replace the serving process again. The reset-only legal action must be
    // reported identically: the tampered store is never repaired implicitly,
    // and a restart is not a reset.
    let second_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the second physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    let mut daemon = spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        second_pid,
        "restart reused the physical daemon process"
    );

    let reset_payload_after_restart = tool_call(
        &home_path,
        &reset_project_path,
        "tracedecay_storage_status",
        &json!({ "include_details": false }),
    );
    assert_reset_required(&reset_payload_after_restart, "after a physical restart");

    let _ = daemon.kill_and_wait();
}
