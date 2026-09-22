//! `tracedecay_work_release_placement` through the MCP `tools/call` path.
//!
//! Release publishes `released` when removal is unblocked and `quarantined`
//! when the fresh observation still names a reason to keep the bytes. It does
//! not delete those bytes. A missing placement, a stale authority version, and
//! a timestamp older than the published transition are typed refusals.

#![cfg(all(feature = "test-transport", unix))]

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::common::fixture::git_run as git;

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, production_composition_fixture,
};

const KEPT_BYTES: &str = "kept placement bytes\n";
/// Byte-identical to the production fixture's committed `src/main.rs`.
const FIXTURE_MAIN: &str = r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#;

async fn call(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"))
}

fn now_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_micros(),
    )
    .expect("current time fits UtcMicros")
}

fn path_arg(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
        .to_owned()
}

fn add_worktree(project_root: &Path, branch: &str, root: &Path) {
    let destination = path_arg(root);
    git(
        project_root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &destination,
            "HEAD",
        ],
    );
}

/// Fields a caller acts on, with minted request identity checked separately.
fn caller_problem(response: &Value) -> Value {
    assert_eq!(response["kind"], "problem", "{response}");
    let problem = &response["value"]["problem"];
    assert_eq!(
        problem["request_id"], response["value"]["request_id"],
        "{response}"
    );
    assert_eq!(problem["trace_id"], problem["request_id"], "{response}");
    let mut stable = problem.clone();
    let Some(object) = stable.as_object_mut() else {
        panic!("release problem is not an object: {response}");
    };
    object.remove("request_id");
    object.remove("trace_id");
    stable
}

fn expected_conflict(code: &str, message: &str) -> Value {
    json!({
        "revision": 1,
        "kind": "conflict",
        "code": code,
        "message": message,
        "diagnostic": {
            "code": code,
            "message": message
        },
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": true,
        "retry": "after_revalidate",
        "retry_scope": "fresh_request",
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "details": [],
        "legal_actions": ["refresh"],
        "coverage": null
    })
}

fn placement(response: &Value) -> Value {
    assert_eq!(response["kind"], "success", "{response}");
    response
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("release success missing placement payload: {response}"))
}

fn clean_in_place(task_id: &str, run_id: &str, occurred_at: i64) -> Value {
    json!({
        "task_id": task_id,
        "run_id": run_id,
        "target": {
            "kind": "clean_in_place",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": true
        },
        "occurred_at": occurred_at
    })
}

fn linked(
    task_id: &str,
    run_id: &str,
    root: &str,
    occurred_at: i64,
    retention: Option<i64>,
) -> Value {
    let mut command = json!({
        "task_id": task_id,
        "run_id": run_id,
        "target": {
            "kind": "linked_worktree",
            "root": root,
            "network_free": true,
            "in_place_acknowledged": false
        },
        "occurred_at": occurred_at
    });
    if let Some(retention) = retention {
        command["retention_eligible_at"] = json!(retention);
    }
    command
}

fn release_command(task_id: &str, run_id: &str, version: u64, occurred_at: i64) -> Value {
    json!({
        "task_id": task_id,
        "run_id": run_id,
        "expected_authority_version": version,
        "occurred_at": occurred_at
    })
}

fn published_placement(
    task_id: &str,
    run_id: &str,
    target: Value,
    state: &str,
    authority_version: u64,
    transitioned_at: i64,
    blockers: Value,
    retention_eligible_at: Value,
) -> Value {
    json!({
        "identity": {
            "task_id": task_id,
            "run_id": run_id
        },
        "target": target,
        "state": state,
        "authority_version": authority_version,
        "transitioned_at": transitioned_at,
        "blockers": blockers,
        "retention_eligible_at": retention_eligible_at
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_placement_publishes_the_observed_state_without_deleting_bytes() {
    let production = production_composition_fixture().await;
    let project_root = production.project_root.clone();
    let server = production
        .harness
        .server(&project_root)
        .expect("production MCP server");
    let at = now_micros();

    let absent = call(
        &server,
        "tracedecay_work_release_placement",
        release_command(
            "task.release-placement.absent",
            "run.release-placement.absent",
            1,
            at,
        ),
    )
    .await;
    assert_eq!(
        caller_problem(&absent),
        json!({
            "revision": 1,
            "kind": "not_found_or_not_authorized",
            "code": "not_found_or_not_authorized",
            "message": "The requested resource was not found or is not authorized",
            "diagnostic": null,
            "committed_receipt": null,
            "owning_layer": "application",
            "terminality": "pre_admission",
            "retryable": false,
            "retry": "never",
            "retry_scope": null,
            "retry_after_millis": null,
            "cancellation_stage": null,
            "unavailable_classification": null,
            "execution_failure_classification": null,
            "details": [],
            "legal_actions": [],
            "coverage": null
        }),
        "{absent}"
    );

    let admitted_at = at + 1_000;
    let admitted = placement(
        &call(
            &server,
            "tracedecay_work_admit_placement",
            clean_in_place(
                "task.release-placement.clean",
                "run.release-placement.clean",
                admitted_at,
            ),
        )
        .await,
    );
    assert_eq!(admitted["state"], "admitted", "{admitted}");
    assert_eq!(admitted["authority_version"], 1, "{admitted}");

    let stale = call(
        &server,
        "tracedecay_work_release_placement",
        release_command(
            "task.release-placement.clean",
            "run.release-placement.clean",
            99,
            admitted_at + 1_000,
        ),
    )
    .await;
    assert_eq!(
        caller_problem(&stale),
        expected_conflict(
            "application.work-placement.authority-conflict",
            "The Work placement authority version changed after this command was prepared.",
        ),
        "{stale}"
    );

    let older = call(
        &server,
        "tracedecay_work_release_placement",
        release_command(
            "task.release-placement.clean",
            "run.release-placement.clean",
            1,
            admitted_at - 1_000,
        ),
    )
    .await;
    assert_eq!(
        caller_problem(&older),
        expected_conflict(
            "application.work-placement.non-monotonic",
            "The Work placement transition is older than the published state.",
        ),
        "{older}"
    );

    let released_at = admitted_at + 2_000;
    let in_place_target = json!({
        "kind": "clean_in_place",
        "root": null,
        "in_place_acknowledged": true,
        "network_free": true
    });
    let released = placement(
        &call(
            &server,
            "tracedecay_work_release_placement",
            release_command(
                "task.release-placement.clean",
                "run.release-placement.clean",
                1,
                released_at,
            ),
        )
        .await,
    );
    assert_eq!(
        released,
        published_placement(
            "task.release-placement.clean",
            "run.release-placement.clean",
            in_place_target,
            "released",
            2,
            released_at,
            json!([]),
            Value::Null,
        ),
        "a stale or older release must not publish; the first current release does"
    );

    let replay = call(
        &server,
        "tracedecay_work_release_placement",
        release_command(
            "task.release-placement.clean",
            "run.release-placement.clean",
            2,
            released_at + 1_000,
        ),
    )
    .await;
    assert_eq!(
        caller_problem(&replay),
        expected_conflict(
            "application.work-placement.already-released",
            "The Work placement was already released.",
        ),
        "{replay}"
    );

    let isolation = project_root
        .parent()
        .expect("production fixture isolation root")
        .to_path_buf();
    let unique_root = isolation.join("placement-unique");
    let clean_root = isolation.join("placement-clean");
    add_worktree(&project_root, "placement-unique", &unique_root);
    add_worktree(&project_root, "placement-clean", &clean_root);
    let kept = unique_root.join("kept-placement-bytes.txt");
    std::fs::write(&kept, KEPT_BYTES).expect("write kept placement bytes");
    git(&unique_root, &["add", "kept-placement-bytes.txt"]);
    git(
        &unique_root,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "keep unique placement bytes",
        ],
    );

    let unique_path = path_arg(&unique_root);
    let unique_at = now_micros();
    let unique_admitted = placement(
        &call(
            &server,
            "tracedecay_work_admit_placement",
            linked(
                "task.release-placement.unique",
                "run.release-placement.unique",
                &unique_path,
                unique_at,
                Some(424_242),
            ),
        )
        .await,
    );
    assert_eq!(unique_admitted["state"], "admitted", "{unique_admitted}");
    assert_eq!(unique_admitted["authority_version"], 1, "{unique_admitted}");

    let quarantined_at = unique_at + 1_000;
    let unique_target = json!({
        "kind": "linked_worktree",
        "root": unique_path,
        "in_place_acknowledged": false,
        "network_free": true
    });
    let quarantined = placement(
        &call(
            &server,
            "tracedecay_work_release_placement",
            release_command(
                "task.release-placement.unique",
                "run.release-placement.unique",
                1,
                quarantined_at,
            ),
        )
        .await,
    );
    assert_eq!(
        quarantined,
        published_placement(
            "task.release-placement.unique",
            "run.release-placement.unique",
            unique_target.clone(),
            "quarantined",
            2,
            quarantined_at,
            json!(["unique_commits"]),
            json!(424_242),
        ),
        "{quarantined}"
    );
    assert_eq!(
        std::fs::read_to_string(&kept).expect("quarantine keeps the placement file"),
        KEPT_BYTES
    );

    let still_held_at = quarantined_at + 1_000;
    let still_held = placement(
        &call(
            &server,
            "tracedecay_work_release_placement",
            release_command(
                "task.release-placement.unique",
                "run.release-placement.unique",
                2,
                still_held_at,
            ),
        )
        .await,
    );
    assert_eq!(
        still_held,
        published_placement(
            "task.release-placement.unique",
            "run.release-placement.unique",
            unique_target,
            "quarantined",
            3,
            still_held_at,
            json!(["unique_commits"]),
            json!(424_242),
        ),
        "a second release re-reads the bytes and keeps the quarantine"
    );
    assert_eq!(
        std::fs::read_to_string(&kept).expect("second release still keeps the file"),
        KEPT_BYTES
    );

    let clean_path = path_arg(&clean_root);
    let clean_at = now_micros();
    let clean_admitted = placement(
        &call(
            &server,
            "tracedecay_work_admit_placement",
            linked(
                "task.release-placement.linked",
                "run.release-placement.linked",
                &clean_path,
                clean_at,
                None,
            ),
        )
        .await,
    );
    assert_eq!(clean_admitted["state"], "admitted", "{clean_admitted}");

    let linked_released_at = clean_at + 1_000;
    let linked_target = json!({
        "kind": "linked_worktree",
        "root": clean_path,
        "in_place_acknowledged": false,
        "network_free": true
    });
    let linked_released = placement(
        &call(
            &server,
            "tracedecay_work_release_placement",
            release_command(
                "task.release-placement.linked",
                "run.release-placement.linked",
                1,
                linked_released_at,
            ),
        )
        .await,
    );
    assert_eq!(
        linked_released,
        published_placement(
            "task.release-placement.linked",
            "run.release-placement.linked",
            linked_target,
            "released",
            2,
            linked_released_at,
            json!([]),
            Value::Null,
        ),
        "{linked_released}"
    );
    assert_eq!(
        std::fs::read_to_string(clean_root.join("src/main.rs")).expect("released worktree remains"),
        FIXTURE_MAIN
    );
}
