//! Production MCP behavior of `tracedecay_fact_store_curate`.
//!
//! The daemon owns run identity, task selection, and effect settlement. These
//! calls go through the real MCP server on the production composition, not a
//! substituted executor.

#![cfg(feature = "test-transport")]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::support::{extract_real_server_text, handle_real_server_tool_call_raw};

const UNKNOWN_FIELD_MESSAGE: &str = "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_curate: operations: unknown field `operations`, expected `fact_review_limit` or `min_confidence_millionths`";

#[tokio::test]
async fn empty_store_curate_skips_and_refuses_caller_authority() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production fact-store MCP server");
    let dashboard_root = server.cg().await.store_layout().dashboard_root.clone();

    // The project's scheduler takes the same curator lock. A held lock is the
    // legal transient skip; wait it out so the assertion is the empty-store
    // terminal, not whichever neighbor happened to be running.
    let mut response = Value::Null;
    for _attempt in 0..10 {
        response = handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_curate",
            json!({
                "fact_review_limit": 7,
                "min_confidence_millionths": 500_000,
            }),
        )
        .await;
        assert!(
            response["error"].is_null(),
            "legal bounds must be admitted: {response}"
        );
        assert_ne!(response["result"]["isError"], json!(true), "{response}");
        let envelope = tool_document(&response);
        if envelope["outcome"]["value"]["payload"]["terminal"]["reason"] != "scheduler_lock_active"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let envelope = tool_document(&response);
    assert_eq!(
        envelope["contract"]["schema_id"],
        "schema.application.retained.fact-store-curate.result"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["outcome"]["outcome"], "effect");
    let effect = &envelope["outcome"]["value"];
    assert_eq!(effect["effect_class"], "administrative");
    assert_eq!(effect["reconciliation"], "reconciled");
    assert_eq!(effect["execution"]["termination"], "completed");
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(
        effect["receipt"]["operation"],
        "use-case.application.retained.fact-store-curate"
    );
    let run = &effect["payload"];
    assert_eq!(run["task"], "memory_curator");
    assert_eq!(run["run_id"], envelope["request_id"]);
    assert_eq!(run["terminal"]["status"], "skipped");
    assert_eq!(run["terminal"]["reason"], "nothing_to_review");
    assert_eq!(run["terminal"]["summary"]["reviewed_count"], 0);
    assert_eq!(run["terminal"]["summary"]["accepted_count"], 0);
    assert_eq!(run["terminal"]["summary"]["rejected_count"], 0);
    // A skip terminal counts the run itself. Reviewed, accepted, and rejected
    // stay at zero because no fact was examined for a mutation.
    assert_eq!(run["terminal"]["summary"]["skipped_count"], 1);
    assert_eq!(run["committed_receipts"], json!([]));

    let run_id = run["run_id"]
        .as_str()
        .expect("curate payload must name the durable run")
        .to_owned();
    let record = ledger_record(&dashboard_root, &run_id);
    assert_eq!(record["trigger"], "application");
    assert_eq!(record["task"], "memory_curator");
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "nothing_to_review");
    assert_eq!(record["reviewed_count"], 0);
    assert_eq!(record["accepted_count"], 0);
    assert_eq!(record["rejected_count"], 0);

    let admitted = application_run_ids(&dashboard_root);

    for bounds in [
        json!({"fact_review_limit": 0, "min_confidence_millionths": 500_000}),
        json!({"fact_review_limit": 1_001, "min_confidence_millionths": 500_000}),
        json!({"fact_review_limit": 7, "min_confidence_millionths": 1_000_001}),
    ] {
        let rejected = handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_curate",
            bounds.clone(),
        )
        .await;
        assert!(
            rejected["error"].is_null(),
            "an out-of-range bound is a typed problem, not a transport error: {rejected}"
        );
        assert_eq!(rejected["result"]["isError"], json!(true), "{rejected}");
        let problem = &rejected["result"]["structuredContent"]["problem"];
        assert_eq!(problem["kind"], "invalid_request");
        assert_eq!(problem["code"], "application.retained.invalid-request");
        assert_eq!(
            problem["message"],
            "The retained operation request is invalid."
        );
        assert_eq!(problem["retry"], "never");
        assert_eq!(problem["retryable"], false);
        assert_eq!(problem["terminality"], "pre_admission");
        assert_eq!(problem["revision"], 1);
        assert_eq!(problem["legal_actions"], json!(["correct_request"]));
        assert!(problem["committed_receipt"].is_null(), "{problem}");
        let document = tool_document(&rejected);
        assert_eq!(
            document["contract"]["schema_id"],
            "schema.application.retained.fact-store-curate.result"
        );
        assert_eq!(document["problem"]["kind"], "invalid_request");
        assert_eq!(
            document["problem"]["code"],
            "application.retained.invalid-request"
        );
        assert_eq!(
            document["problem"]["message"],
            "The retained operation request is invalid."
        );
    }

    let forbidden = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_curate",
        json!({
            "fact_review_limit": 7,
            "operations": [],
        }),
    )
    .await;
    assert_eq!(forbidden["error"]["code"], -32603, "{forbidden}");
    assert_eq!(forbidden["error"]["message"], UNKNOWN_FIELD_MESSAGE);
    assert_eq!(
        forbidden["error"]["data"]["tool"],
        "tracedecay_fact_store_curate"
    );
    assert!(forbidden["result"].is_null(), "{forbidden}");

    assert_eq!(
        application_run_ids(&dashboard_root),
        admitted,
        "rejected curate calls must not append an application run"
    );

    fixture.harness.shutdown().await;
}

fn tool_document(response: &Value) -> Value {
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("fact_store_curate content must be JSON")
}

fn ledger_record(dashboard_root: &Path, run_id: &str) -> Value {
    ledger_records(dashboard_root)
        .into_iter()
        .find(|record| record["run_id"] == run_id)
        .unwrap_or_else(|| panic!("ledger must record run {run_id}"))
}

fn application_run_ids(dashboard_root: &Path) -> BTreeSet<String> {
    ledger_records(dashboard_root)
        .into_iter()
        .filter(|record| record["trigger"] == "application")
        .map(|record| record["run_id"].as_str().expect("ledger run_id").to_owned())
        .collect()
}

fn ledger_records(dashboard_root: &Path) -> Vec<Value> {
    let path = dashboard_root.join("automation_runs.jsonl");
    let ledger = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("automation run ledger {path:?} must be readable: {error}"));
    ledger
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("ledger line must be JSON"))
        .collect()
}
