//! A daemon-managed test run a session requested is recorded durably at the
//! moment it runs, so a physically restarted daemon still serves it on that
//! session's Loom lane.

use crate::common;
use crate::private_route_restart_acceptance::await_published_code_index;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay::daemon::{call_default_tool, tool_json_payload};
use tracedecay_daemon_protocol::DaemonHandshake;

const SESSION: &str = "loom-restart-session-001";

fn write_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::create_dir_all(project.join("tests")).expect("fixture test directory");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"loom_restart\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("fixture manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn greeting(name: &str) -> String {\n    format!(\"hello, {name}\")\n}\n",
    )
    .expect("fixture library");
    std::fs::write(
        project.join("tests/greeting.rs"),
        "use loom_restart::greeting;\n\n\
         #[test]\nfn greets_by_name() {\n    assert_eq!(greeting(\"loom\"), \"hello, loom\");\n}\n\n\
         #[test]\nfn greets_politely() {\n    assert_eq!(greeting(\"loom\"), \"good day, loom\");\n}\n",
    )
    .expect("fixture tests");
    std::fs::write(project.join(".gitignore"), "/target\nCargo.lock\n")
        .expect("ignore cargo artifacts so a managed run cannot dirty the sealed source revision");
    common::fixture::git_run(project, &["init", "-q"]);
    common::fixture::git_run(project, &["add", "."]);
    common::fixture::git_run(
        project,
        &["commit", "-qm", "managed test-run restart fixture"],
    );
}

/// The Codex rollout the daemon admits as the session that requests the run.
fn write_codex_session(home: &Path, project: &Path) {
    let sessions = home.join(".codex/sessions/2026/09/28");
    std::fs::create_dir_all(&sessions).expect("isolated Codex sessions directory");
    let records = [
        json!({"timestamp": "2026-09-28T16:00:00.000Z", "type": "session_meta",
               "payload": {"id": SESSION, "cwd": project, "model": "gpt-5.6"}}),
        json!({"timestamp": "2026-09-28T16:00:01.000Z", "type": "event_msg",
               "payload": {"type": "user_message", "message": "Run the affected greeting tests"}}),
    ];
    let rollout: Vec<String> = records.iter().map(Value::to_string).collect();
    std::fs::write(
        sessions.join(format!("rollout-{SESSION}.jsonl")),
        rollout.join("\n") + "\n",
    )
    .expect("isolated Codex rollout");
}

/// The daemon runs `cargo test` for the fixture: it needs the toolchain the
/// suite itself was built with, ahead of the hermetic system directories, and
/// none of the suite's own target directory or the shared cargo home's
/// compiler wrapper (an empty `RUSTC_WRAPPER` overrides a configured one).
fn spawn_daemon(environment: &common::IsolatedHome) -> common::DaemonProcess {
    let cargo = PathBuf::from(std::env::var_os("CARGO").expect("cargo test sets CARGO"));
    let toolchain_bin = cargo
        .parent()
        .expect("cargo binary directory")
        .to_path_buf();
    common::spawn_tracedecay_daemon_with(environment.home(), |command| {
        environment.apply_toolchain_env(command);
        command
            .env("PATH", common::hermetic_path(&[toolchain_bin]))
            .env("RUSTC_WRAPPER", "")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR");
    })
}

async fn mcp_tool(
    environment: &common::IsolatedHome,
    handshake: &DaemonHandshake,
    name: &str,
    mut arguments: Value,
) -> Value {
    arguments["format"] = json!("json");
    let result = call_default_tool(environment.profile(), handshake, name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{name} daemon call: {error}"));
    tool_json_payload(&result, name).unwrap_or_else(|error| panic!("{name} payload: {error}"))
}

/// The Loom temporal read once the restarted daemon's session authority has
/// mounted; the dashboard answers `loading` until then.
fn loom_temporal(dashboard: &str) -> Value {
    let agent = common::http_agent();
    let url = format!("{dashboard}/api/loom/temporal?limit=200");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, envelope) = common::get_json(&agent, &url);
        assert_eq!(status, 200, "{envelope}");
        if envelope["payload"]["available"] == json!(true) {
            return envelope;
        }
        assert_eq!(envelope["domain_state"], json!("loading"), "{envelope}");
        assert!(Instant::now() < deadline, "Loom never mounted: {envelope}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_attributed_test_run_survives_a_physical_daemon_restart_on_loom() {
    let (environment, project) = common::IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    write_project(&project);
    write_codex_session(environment.home(), &project);
    let mut daemon = spawn_daemon(&environment);
    let initialized = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert!(initialized.status.success(), "init failed: {initialized:?}");
    await_published_code_index(environment.home(), &project);
    let handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(project.clone()),
        None,
        false,
        false,
    )
    .expect("daemon handshake");
    // Session stores mount with the full project server, after the code
    // index; until then the owner answers the typed mounting refusal.
    let deadline = Instant::now() + Duration::from_secs(60);
    let ingest = loop {
        let ingest = mcp_tool(
            &environment,
            &handshake,
            "tracedecay_hook_runtime",
            json!({"action": "ingest_transcript", "provider": "codex", "user_scope": false}),
        )
        .await;
        if ingest["problem"]["code"] != json!("application.runtime.mounting") {
            break ingest;
        }
        assert!(
            Instant::now() < deadline,
            "session stores never mounted: {ingest}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(ingest["completed"], json!(true), "{ingest}");

    let attributed = mcp_tool(
        &environment,
        &handshake,
        "tracedecay_run_affected_tests",
        json!({"changed_paths": ["tests/greeting.rs"], "session_id": SESSION,
               "timeout_secs": 120, "max_tests": 5}),
    )
    .await;
    assert_eq!(
        (
            &attributed["exit_code"],
            &attributed["passed"],
            &attributed["failed"]
        ),
        (&json!(101), &json!(1), &json!(1)),
        "{attributed}"
    );
    let terminal = &attributed["terminal"];
    assert_eq!(terminal["session_id"], json!(SESSION), "{attributed}");
    let started_at = terminal["receipt"]["started_at"]
        .as_i64()
        .expect("recorded start");
    let ended_at = terminal["receipt"]["ended_at"]
        .as_i64()
        .expect("recorded end");
    let sessionless = mcp_tool(
        &environment,
        &handshake,
        "tracedecay_run_affected_tests",
        json!({"changed_paths": ["src/lib.rs"], "timeout_secs": 120, "max_tests": 1}),
    )
    .await;
    assert_eq!(
        sessionless["terminal"]["session_id"],
        Value::Null,
        "{sessionless}"
    );

    let first_pid = daemon.id();
    daemon
        .kill_and_wait()
        .expect("force-stop and reap the recording daemon");
    daemon = spawn_daemon(&environment);
    assert_ne!(daemon.id(), first_pid, "restart reused the daemon process");
    await_published_code_index(environment.home(), &project);

    let started = mcp_tool(
        &environment,
        &handshake,
        "tracedecay_dashboard",
        json!({"host": "127.0.0.1", "port": 0}),
    )
    .await;
    let dashboard =
        common::dashboard_api_base_url(started["url"].as_str().expect("dashboard launch URL"));
    let envelope = tokio::task::spawn_blocking(move || loom_temporal(&dashboard))
        .await
        .expect("Loom temporal read");
    call_default_tool(
        environment.profile(),
        &handshake,
        "tracedecay_dashboard",
        json!({"action": "stop"}),
    )
    .await
    .expect("stop the dashboard listener");
    let runs: Vec<&Value> = envelope["payload"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("Loom events: {envelope}"))
        .iter()
        .filter(|event| event["kind"] == "test_run")
        .collect();
    assert_eq!(
        runs,
        vec![&json!({
            "kind": "test_run",
            "provider": "codex",
            "session_id": SESSION,
            "operation_id": terminal["operation_id"],
            "recorded_at": started_at.div_euclid(1_000_000),
            "started_at_micros": started_at,
            "outcome": {
                "finished_at_micros": ended_at,
                "termination": "completed",
                "exit_code": 101,
                "passed": 1,
                "failed": 1,
                "ignored": 0,
            },
        })],
        "the restarted daemon serves the recorded run on the requesting session's lane"
    );
    let status = envelope["payload"]["source_statuses"]
        .as_array()
        .and_then(|statuses| {
            statuses
                .iter()
                .find(|status| status["id"] == "session_test")
        })
        .unwrap_or_else(|| panic!("missing test-run source: {envelope}"));
    assert_eq!(
        (
            &status["state"],
            &status["coverage"]["eligible"],
            &status["coverage"]["matched"],
            &status["coverage"]["omitted"],
        ),
        (&json!("partial"), &json!(2), &json!(1), &json!(1)),
        "the sessionless run is counted unattributed, not dropped: {status}"
    );
    daemon.kill_and_wait().expect("stop the restarted daemon");
}
