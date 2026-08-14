mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tracedecay::daemon::{DaemonHandshake, call_default_tool};

fn initialize_project(home: &Path, project: &Path, marker: &str) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/route_marker.rs"),
        format!("pub const ROUTE_RESTART_MARKER: &str = \"{marker}\";\n"),
    )
    .expect("project route marker");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("initialize fixture git repository");
    assert!(git.status.success(), "git init failed: {git:?}");
    common::initialize_tracedecay_cli_project(home, project);
}

fn admitted_project_id(home: &Path, project: &Path) -> String {
    let project_arg = project.to_string_lossy().into_owned();
    let output = common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "storage_status",
            "--args",
            r#"{"include_details":false}"#,
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("read admitted project identity");
    assert!(
        output.status.success(),
        "storage_status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value =
        serde_json::from_slice(&output.stdout).expect("storage status application envelope");
    envelope["scope"]["project_id"]
        .as_str()
        .unwrap_or_else(|| panic!("storage status omitted project identity: {envelope}"))
        .to_owned()
}

fn project_handshake(project: &Path) -> DaemonHandshake {
    DaemonHandshake::for_current_client(Some(project.to_path_buf()), None, false, false)
        .expect("project daemon handshake")
}

async fn assert_selected_target_source(
    caller: &DaemonHandshake,
    target_project_id: &str,
    target_marker: &str,
    caller_marker: &str,
) {
    let result = call_default_tool(
        caller,
        "tracedecay_grep",
        json!({
            "pattern": "ROUTE_RESTART_MARKER",
            "fixed_strings": true,
            "project_selector": {"project_id": target_project_id},
            "format": "json"
        }),
    )
    .await
    .expect("selected project grep");
    let payload = tracedecay::daemon::tool_json_payload(&result, "tracedecay_grep")
        .expect("selected grep JSON payload");
    let results = payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("selected grep omitted results: {payload}"));
    assert!(
        results.iter().any(|hit| hit["text"]
            .as_str()
            .is_some_and(|text| text.contains(target_marker))),
        "selected target source was not returned: {payload}"
    );
    assert!(
        results.iter().all(|hit| !hit["text"]
            .as_str()
            .is_some_and(|text| text.contains(caller_marker))),
        "selected route fell back to caller project source: {payload}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_project_source_route_survives_physical_daemon_restart() {
    const CALLER_MARKER: &str = "private-route-caller-alpha";
    const TARGET_MARKER: &str = "private-route-target-beta";

    let (environment, project_a) = common::IsolatedEnv::acquire().await;
    let project_b = environment.scratch().join("project-b");
    let mut daemon = common::spawn_tracedecay_daemon(environment.home());
    initialize_project(environment.home(), &project_a, CALLER_MARKER);
    initialize_project(environment.home(), &project_b, TARGET_MARKER);
    let project_a_id = admitted_project_id(environment.home(), &project_a);
    let project_b_id = admitted_project_id(environment.home(), &project_b);
    assert_ne!(project_a_id, project_b_id);
    let caller = project_handshake(&project_a);

    assert_selected_target_source(&caller, &project_b_id, TARGET_MARKER, CALLER_MARKER).await;

    let first_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap first physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    daemon = common::spawn_tracedecay_daemon(environment.home());
    assert_ne!(
        daemon.id(),
        first_pid,
        "restart reused the physical daemon process"
    );
    assert_eq!(
        admitted_project_id(environment.home(), &project_b),
        project_b_id,
        "target project identity changed after restart"
    );
    assert_eq!(
        admitted_project_id(environment.home(), &project_a),
        project_a_id,
        "caller project identity changed after restart"
    );
    assert_selected_target_source(&caller, &project_b_id, TARGET_MARKER, CALLER_MARKER).await;
}
