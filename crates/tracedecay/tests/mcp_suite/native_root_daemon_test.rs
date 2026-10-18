//! `tracedecay daemon run` drives its root on the native runtime. Its
//! remaining Tokio sites reach the one Tokio runtime it owns through explicit
//! handles. A tool call, an MCP session and a dashboard request succeed
//! without a writer wait parking a native worker, and the stop settles every
//! shutdown owner.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{self, tracedecay_command_with_home};
use crate::serve_harness::{ServeSession, init_project_with_file, serve_command};

const MARKER: &str = "native_root_daemon_marker";

/// The value of `field` on the daemon's runtime hazards receipt.
fn runtime_hazard(log: &str, field: &str) -> u64 {
    let line = log
        .lines()
        .find(|line| line.contains("event=daemon_runtime_hazards "))
        .unwrap_or_else(|| panic!("the daemon must log its runtime hazards at exit; log={log}"));
    line.split(' ')
        .find_map(|pair| pair.strip_prefix(&format!("{field}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("runtime hazards line lacks {field}: {line}"))
}

fn start_hosted_dashboard(session: &mut ServeSession) -> (u16, String) {
    let started = session.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_dashboard",
            "arguments": { "host": "127.0.0.1", "port": 0, "format": "json" }
        }
    }));
    let text = started["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("the dashboard tool must answer with text: {started}"));
    let payload: Value = serde_json::from_str(text).expect("dashboard payload");
    assert_eq!(payload["status"], json!("started"), "{payload}");
    let port = payload["port"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .expect("bound dashboard port");
    let url = payload["url"].as_str().expect("launch URL").to_owned();
    (port, url)
}

#[tokio::test]
async fn daemon_on_the_native_root_serves_tool_mcp_and_dashboard_then_stops_clean() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &format!("pub fn {MARKER}() {{}}\n")).await;
    let log_dir = TempDir::new().unwrap();
    let log_path = log_dir.path().join("daemon.log");
    let log_file = std::fs::File::create(&log_path).expect("daemon log file");
    let mut daemon = common::spawn_tracedecay_daemon_with(home.path(), |command| {
        command.stderr(log_file);
    });

    let mut session = ServeSession::start(serve_command(
        home.path(),
        project.path(),
        Some(project.path().as_os_str()),
    ));
    session.request(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
    session.await_project_ready();

    let tool = tracedecay_command_with_home(home.path())
        .current_dir(project.path())
        .args(["tool", "tracedecay_search", "--args"])
        .arg(json!({ "query": MARKER }).to_string())
        .stdin(Stdio::null())
        .output()
        .expect("tracedecay tool runs");
    assert!(
        tool.status.success() && String::from_utf8_lossy(&tool.stdout).contains(MARKER),
        "the tool call must find the indexed symbol\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tool.stdout),
        String::from_utf8_lossy(&tool.stderr)
    );

    let (_, launch_url) = start_hosted_dashboard(&mut session);

    let agent = tracedecay_application::http_agent::http_agent(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(10)))
            .build(),
    );
    let launched = agent.get(&launch_url).call().expect("launch URL response");
    assert_eq!(launched.status().as_u16(), 303);
    assert!(launched.headers().get("set-cookie").is_none());
    let mut capabilities = agent
        .get(&format!(
            "{}/api/capabilities",
            common::dashboard_api_base_url(&launch_url)
        ))
        .call()
        .expect("dashboard capabilities response");
    assert_eq!(capabilities.status().as_u16(), 200);
    let body: Value =
        serde_json::from_str(&capabilities.body_mut().read_to_string().unwrap()).unwrap();
    assert_eq!(body["name"], json!("tracedecay-dashboard"), "{body}");

    let serve = session.finish(Duration::from_secs(20));
    assert!(
        serve.status.success(),
        "the MCP session must end clean: {serve:?}"
    );

    // SAFETY: signals only the daemon child this test spawned.
    assert_eq!(
        unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let exit = daemon
        .wait_for_exit(Duration::from_secs(120))
        .expect("wait for daemon shutdown")
        .expect("the daemon exits after SIGTERM");
    let log = std::fs::read_to_string(&log_path).expect("daemon log");
    assert!(
        exit.success(),
        "the daemon must stop clean: {exit}; log={log}"
    );

    assert_eq!(
        runtime_hazard(&log, "native_thread_writer_waits"),
        0,
        "a writer wait parked a native worker; log={log}"
    );
    for unsettled in [
        "outcome=background_task_unfinished",
        "outcome=client_drain_timeout",
        "outcome=project_server_shutdown_incomplete",
        "there is no reactor running",
        "panicked at",
    ] {
        assert!(!log.contains(unsettled), "{unsettled}; log={log}");
    }
    assert!(
        log.contains("outcome=store_close_complete"),
        "shutdown must reach its terminal store close; log={log}"
    );
}
