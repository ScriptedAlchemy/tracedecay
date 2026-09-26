//! Queries against a worktree whose code index is parked, through the shipped
//! daemon.
//!
//! A committed source file the daemon cannot read fails every reconcile the
//! same way, so the worker parks convergence until the operator acts. A query
//! that answers with a retryable refusal there is retried forever; it must
//! carry the park's cause and remedy as typed `detail` on every surface (MCP,
//! the CLI, and HTTP) and say that retrying cannot help.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, git, initialize_tracedecay, search, stop_daemon_gracefully, tool,
};
use crate::common::{
    EnvVarGuard, IsolatedEnv, daemon_authority_path, daemon_socket_path, http_agent_with_timeout,
    response_to_json, spawn_tracedecay_daemon_with, tracedecay_command_with_home,
};

const CAUSE: &str = "code-index repository status failed: code-index classification: IO error \
    while writing blob or reading file metadata or changing filetype";
const REMEDY: &str = "indexing this worktree fails the same way on every pass over unchanged \
    source; fix what the named failure points at, then run `tracedecay sync` to retry; if it \
    names an internal indexing contract, run `tracedecay upgrade` (a restarted daemon retries \
    automatically) and report the failure if it persists";

#[tokio::test]
async fn parked_worktree_queries_carry_the_park_and_are_not_retryable() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn alpha() {}\nmod model;\n",
    )
    .expect("fixture source");
    fs::write(
        project.join("src/model.rs"),
        "pub struct Gamma;\npub fn delta() {}\n",
    )
    .expect("fixture model source");
    git(&project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(&project, "parked fixture");
    let unreadable = project.join("src/model.rs");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
        .expect("make the committed source unreadable");

    let socket = daemon_socket_path(environment.home());
    let log_path = environment.scratch().join("code-index-park-daemon.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");

    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    let searched = loop {
        let searched = search(&socket, &handshake, "alpha").await;
        if searched["freshness"]["indexing"]["staleness_state"] == "parked" {
            break searched;
        }
        assert!(
            Instant::now() < deadline,
            "the unreadable worktree never parked: {searched}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(searched["status"], "unavailable", "{searched}");
    let parked = &searched["freshness"]["indexing"]["parked"];
    assert_eq!(
        (
            &parked["reason"],
            &parked["remediation"],
            &parked["retries_on_wake"],
        ),
        (&json!(CAUSE), &json!(REMEDY), &json!(false)),
        "search must carry the park, not only a retryable lane reason: {searched}"
    );
    let parked_detail = json!({
        "kind": "parked",
        "cause": CAUSE,
        "remedy": REMEDY,
        "retries_on_wake": false,
    });
    assert_eq!(
        searched["detail"], parked_detail,
        "search must carry the park as typed detail: {searched}"
    );

    let request = json!({
        "query": "alpha",
        "scope": { "path_prefix": null },
        "lazy_index_ignored_dependencies": false,
        "meta": { "projection": "summary", "order": "relevance" },
    });
    let mut mcp_arguments = request.clone();
    mcp_arguments["format"] = json!("json");
    let refused = tool(
        &socket,
        &handshake,
        "tracedecay_code_symbol_search",
        mcp_arguments.clone(),
    )
    .await;
    let problem = &refused["problem"];
    assert_eq!(
        (
            &problem["code"],
            &problem["retryable"],
            &problem["retry"],
            &problem["legal_actions"],
        ),
        (
            &json!("application.code-index.parked"),
            &json!(false),
            &json!("never"),
            &json!(["reconcile"]),
        ),
        "symbol search on a parked worktree must refuse without retry: {refused}"
    );
    assert_eq!(
        problem["message"],
        format!("The code index for this worktree is parked; remedy: {REMEDY}; cause: {CAUSE}"),
        "the refusal must name the park's remedy and cause: {refused}"
    );
    assert_eq!(problem["detail"], parked_detail, "MCP content: {refused}");
    let structured = tracedecay::daemon::call_tool(
        &socket,
        &handshake,
        "tracedecay_code_symbol_search",
        mcp_arguments,
    )
    .await
    .expect("MCP symbol search transport");
    assert_eq!(
        structured["structuredContent"]["problem"]["detail"], parked_detail,
        "the structured MCP problem must carry the detail: {structured}"
    );

    let cli = cli_symbol_search(environment.home(), &project, &request, true);
    assert_eq!(
        (&cli["problem"]["detail"], &cli["problem"]["message"]),
        (&parked_detail, &problem["message"]),
        "CLI --json: {cli}"
    );
    let cli_text = cli_symbol_search_text(environment.home(), &project, &request);
    for line in [
        format!("- Parked cause: {CAUSE}"),
        "- Retries on wake: false".to_owned(),
    ] {
        assert!(
            cli_text.contains(&line),
            "the CLI must print `{line}` as a labelled line:\n{cli_text}"
        );
    }

    let (status, http) = http_symbol_search(environment.home(), &project_id, &request);
    assert_eq!(
        status, 503,
        "a parked refusal is unavailable over HTTP: {http}"
    );
    assert_eq!(
        (
            &http["value"]["problem"]["detail"],
            &http["value"]["problem"]["message"]
        ),
        (&parked_detail, &problem["message"]),
        "HTTP problem body: {http}"
    );

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644))
        .expect("restore the source mode");
    stop_daemon_gracefully(&mut daemon);
}

fn cli_symbol_search_output(home: &Path, project: &Path, request: &Value, json: bool) -> String {
    let project_arg = project.to_string_lossy().into_owned();
    let request = request.to_string();
    let mut command = tracedecay_command_with_home(home);
    command.current_dir(project).args([
        "tool",
        "--project",
        project_arg.as_str(),
        "code_symbol_search",
        "--args",
        request.as_str(),
    ]);
    if json {
        command.arg("--json");
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay tool code_symbol_search");
    assert!(
        !output.status.success(),
        "a parked refusal must fail the CLI\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("CLI stdout is UTF-8")
}

fn cli_symbol_search(home: &Path, project: &Path, request: &Value, json: bool) -> Value {
    let stdout = cli_symbol_search_output(home, project, request, json);
    serde_json::from_str(stdout.lines().next().unwrap_or_default())
        .unwrap_or_else(|error| panic!("CLI --json printed no envelope ({error}):\n{stdout}"))
}

fn cli_symbol_search_text(home: &Path, project: &Path, request: &Value) -> String {
    cli_symbol_search_output(home, project, request, false)
}

fn http_symbol_search(home: &Path, project_id: &str, request: &Value) -> (u16, Value) {
    let authority: Value = serde_json::from_slice(
        &fs::read(daemon_authority_path(&home.join(".tracedecay")))
            .expect("published daemon authority"),
    )
    .expect("daemon authority JSON");
    let endpoint = authority["http_application_endpoint"]
        .as_str()
        .expect("daemon HTTP application endpoint");
    let token = authority["auth_token"].as_str().expect("daemon token");
    let base = format!("http://{endpoint}");
    let url = format!("{base}/projects/{project_id}/application/code/code_symbol_search");
    let response = http_agent_with_timeout(RECEIPT_TIMEOUT)
        .post(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("origin", base.as_str())
        .header("content-type", "application/json")
        .send_json(request)
        .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
    response_to_json(response)
}
