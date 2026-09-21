//! Host-visible behavior of `tracedecay_session_refresh_status`.
//!
//! Every call is a production-composition JSON-RPC `tools/call`. The
//! assertions name the envelope a host reads, not scheduler or store calls.

use std::path::Path;
use std::time::Duration;

use crate::common::fixture::git_run as git;

use serde_json::{Value, json};

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::fixture;
use crate::support::{GLOBAL_DB_ENV_LOCK, HomeEnvGuard, test_temp_dir};

const TOOL: &str = "tracedecay_session_refresh_status";
const SESSION_ID: &str = "session.status-proof";
const OTHER_SESSION_ID: &str = "session.status-proof-other";

struct HostAnswer {
    refused: bool,
    body: Value,
}

fn refresh_arguments(session_id: &str, handle: Option<&str>) -> Value {
    let mut arguments = json!({
        "scope": {"kind": "profile"},
        "session": {"id": session_id},
        "source": {"scope": "codex"},
        "target": {
            "temporal_mode": {"kind": "current"},
            "grain": "session",
            "frontier": {"observed_through": 0, "committed_through": 0}
        },
        "format": "json"
    });
    if let Some(handle) = handle {
        arguments["handle"] = json!(handle);
    }
    arguments
}

async fn call_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> HostAnswer {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool} must answer as a tool result, not a JSON-RPC error: {response:?}"
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} omitted its tool result"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} omitted text content: {result}"));
    let body = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool} did not return JSON: {error}; text={text}"));
    HostAnswer {
        refused: result["isError"] == true,
        body,
    }
}

fn status_payload(answer: &HostAnswer) -> &Value {
    answer
        .body
        .pointer("/outcome/value/payload")
        .unwrap_or_else(|| {
            panic!(
                "status answer was not an evidence envelope: {}",
                answer.body
            )
        })
}

fn assert_status_contract(answer: &HostAnswer) {
    assert!(
        !answer.refused,
        "a typed status answer is evidence, not an MCP refusal: {}",
        answer.body
    );
    assert_eq!(
        answer.body["contract"]["schema_id"],
        "schema.application.retained.session-refresh-status.result"
    );
    assert_eq!(answer.body["contract"]["schema_revision"], 1);
    assert_eq!(answer.body["outcome"]["outcome"], "evidence");
}

fn assert_invalid_request(answer: &HostAnswer) {
    assert_problem(
        answer,
        json!({
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            },
            "retry": "never",
            "retryable": false,
            "retry_scope": null,
            "legal_actions": ["correct_request"],
            "terminality": "pre_admission",
            "owning_layer": "application",
            "revision": 1,
            "committed_receipt": null
        }),
    );
}

fn assert_problem(answer: &HostAnswer, expected: Value) {
    assert!(
        answer.refused,
        "this status call must refuse as an MCP tool error: {}",
        answer.body
    );
    assert_eq!(
        json!({
            "kind": answer.body["problem"]["kind"],
            "code": answer.body["problem"]["code"],
            "message": answer.body["problem"]["message"],
            "diagnostic": answer.body["problem"]["diagnostic"],
            "retry": answer.body["problem"]["retry"],
            "retryable": answer.body["problem"]["retryable"],
            "retry_scope": answer.body["problem"]["retry_scope"],
            "legal_actions": answer.body["problem"]["legal_actions"],
            "terminality": answer.body["problem"]["terminality"],
            "owning_layer": answer.body["problem"]["owning_layer"],
            "revision": answer.body["problem"]["revision"],
            "committed_receipt": answer.body["problem"]["committed_receipt"],
        }),
        expected
    );
}

fn assert_not_found_or_not_authorized(answer: &HostAnswer) {
    assert_problem(
        answer,
        json!({
            "kind": "not_found_or_not_authorized",
            "code": "not_found_or_not_authorized",
            "message": "The requested resource was not found or is not authorized",
            "diagnostic": null,
            "retry": "never",
            "retryable": false,
            "retry_scope": null,
            "legal_actions": [],
            "terminality": "pre_admission",
            "owning_layer": "application",
            "revision": 1,
            "committed_receipt": null
        }),
    );
}

fn assert_lookup(answer: &HostAnswer, outcome: &str, code: &str, message: &str) {
    assert_status_contract(answer);
    let payload = status_payload(answer);
    assert_eq!(
        payload,
        &json!({
            "outcome": outcome,
            "scope": "profile",
            "tool": TOOL,
            "progress": null,
            "receipt": null,
            "error": {
                "code": code,
                "message": message
            }
        })
    );
}

/// Status reads the handle the host already holds. A missing or blank handle
/// is an invalid request. A handle that is not a refresh token is refused as
/// not found. A well-formed token the daemon does not hold is stale evidence.
/// Presenting a finished handle under another session id does not rebind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_refresh_status_reports_the_handle_the_host_holds() {
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("isolated home");
    let _home_guard = HomeEnvGuard::set(&home);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("project");
    fixture::write_indexed_fixture_sources(&project);
    git(&project, &["init", "-q"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "session refresh status fixture",
        ],
    );

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition harness");

    let omitted = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(SESSION_ID, None),
    )
    .await;
    assert_invalid_request(&omitted);

    let blank = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(SESSION_ID, Some("  ")),
    )
    .await;
    assert_invalid_request(&blank);

    let unknown = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(SESSION_ID, Some("refresh-handle")),
    )
    .await;
    assert_not_found_or_not_authorized(&unknown);

    let stale_token = format!("srh_{}", "0".repeat(64));
    let stale = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(SESSION_ID, Some(&stale_token)),
    )
    .await;
    assert_lookup(
        &stale,
        "stale",
        "refresh_handle_stale",
        "the refresh handle is no longer current",
    );

    let begun = call_tool(
        &harness,
        &project,
        "tracedecay_session_refresh_begin",
        refresh_arguments(SESSION_ID, None),
    )
    .await;
    assert!(
        !begun.refused,
        "begin is setup for the status read: {}",
        begun.body
    );
    let begin_payload = begun
        .body
        .pointer("/outcome/value/payload")
        .unwrap_or_else(|| panic!("begin was not an effect envelope: {}", begun.body));
    let handle = begin_payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted the opaque handle: {begin_payload}"))
        .to_owned();
    let operation_id = begin_payload["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("begin omitted the operation id: {begin_payload}"))
        .to_owned();

    let completed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = call_tool(
                &harness,
                &project,
                TOOL,
                refresh_arguments(SESSION_ID, Some(&handle)),
            )
            .await;
            assert_status_contract(&status);
            let payload = status_payload(&status).clone();
            match payload["outcome"].as_str() {
                Some("complete") => break payload,
                Some("running") => {
                    assert_eq!(
                        json!({
                            "scope": payload["scope"],
                            "tool": payload["tool"],
                            "receipt": payload["receipt"],
                            "error": payload["error"],
                        }),
                        json!({
                            "scope": "profile",
                            "tool": TOOL,
                            "receipt": null,
                            "error": null,
                        }),
                        "{payload}"
                    );
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                _ => panic!("status returned an unexpected outcome: {payload}"),
            }
        }
    })
    .await
    .expect("session refresh status did not reach a terminal receipt");

    let terminal_at = completed["receipt"]["terminal_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("terminal receipt omitted its clock: {completed}"));
    assert!(
        terminal_at > 0,
        "terminal clock must be a recorded instant, got {terminal_at}"
    );
    assert_eq!(
        completed,
        json!({
            "outcome": "complete",
            "scope": "profile",
            "tool": TOOL,
            "progress": null,
            "receipt": {
                "operation_id": operation_id,
                "session_id": SESSION_ID,
                "frontier": {"observed_through": 0, "committed_through": 0},
                "coverage": {"visible": 0, "hidden": 0, "unknown": 0, "redacted": 0},
                "source_coverage": [{
                    "source_id": "session.status-proof:codex",
                    "observed_frontier": 0,
                    "committed_frontier": 0,
                    "target_watermark": 0,
                    "request": {"mode": {"kind": "current"}},
                    "covered_intervals": [],
                    "missing_intervals": [],
                    "state": "fresh",
                    "reason": {"kind": "caught_up"}
                }],
                "state": "complete",
                "failure_code": null,
                "terminal_at": terminal_at
            },
            "error": null
        })
    );

    let repeated = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(SESSION_ID, Some(&handle)),
    )
    .await;
    assert_status_contract(&repeated);
    assert_eq!(
        status_payload(&repeated),
        &completed,
        "a second status read must return the same terminal receipt"
    );

    let foreign = call_tool(
        &harness,
        &project,
        TOOL,
        refresh_arguments(OTHER_SESSION_ID, Some(&handle)),
    )
    .await;
    assert_status_contract(&foreign);
    assert_eq!(
        status_payload(&foreign),
        &completed,
        "presenting the finished handle for another session returns the same receipt"
    );

    harness.shutdown().await;
}
