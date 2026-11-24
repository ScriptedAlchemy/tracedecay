//! Production MCP behavior of `tracedecay_fact_store_curate`.
//!
//! The daemon owns run identity, task selection, and effect settlement. These
//! calls go through the real MCP server on the production composition, not a
//! substituted executor.

#![cfg(feature = "test-transport")]

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_automation_runtime::automation::registered_run_store::RegisteredAutomationRunStore;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunStatus, AutomationTrigger,
};

use crate::support::{
    application_invalid_request_error, extract_real_server_text, handle_real_server_tool_call_raw,
    route_refusal,
};

const UNKNOWN_FIELD_DETAIL: &str = "operations: unknown field `operations`, expected `fact_review_limit` or `min_confidence_millionths`";

#[tokio::test]
async fn empty_store_curate_skips_and_refuses_caller_authority() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production fact-store MCP server");
    let runs = server.cg_snapshot().await.automation_run_store().unwrap();

    // The project's scheduler takes the same curator lock. A held lock is the
    // legal transient skip; wait it out so the assertion is the empty-store
    // terminal, not whichever neighbor happened to be running.
    let mut envelope = Value::Null;
    let mut record = Value::Null;
    for _attempt in 0..10 {
        let response = handle_real_server_tool_call_raw(
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
        envelope = tool_document(&response);
        let run_id = envelope["request_id"]
            .as_str()
            .expect("request id")
            .to_owned();
        record = settled_run(&server, &run_id).await;
        if record["error"] != "scheduler_lock_active" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    assert_eq!(
        envelope["contract"]["schema_id"],
        "schema.application.retained.fact-store-curate.result"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["outcome"]["outcome"], "effect");
    let effect = &envelope["outcome"]["value"];
    assert_eq!(effect["effect_class"], "administrative");
    // The receipt is issued at admission, before the run commits anything.
    assert_eq!(effect["reconciliation"], "pending");
    assert_eq!(effect["execution"]["termination"], "effect_unknown");
    assert_eq!(effect["receipt"]["outcome"], "effect_unknown");
    assert_eq!(effect["receipt"]["committed_state"], Value::Null);
    assert_eq!(
        effect["receipt"]["operation"],
        "use-case.application.retained.fact-store-curate"
    );
    let receipt = &effect["payload"];
    assert_eq!(
        receipt,
        &json!({
            "run_id": envelope["request_id"],
            "task": "memory_curator",
            "request_digest": receipt["request_digest"],
            "state": "started",
        })
    );
    assert!(
        receipt["request_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{receipt}"
    );

    let run_id = receipt["run_id"]
        .as_str()
        .expect("curate receipt must name the durable run")
        .to_owned();
    assert_eq!(record["run_id"], run_id);
    assert_eq!(record["trigger"], "application");
    assert_eq!(record["task"], "memory_curator");
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "nothing_to_review");
    assert_eq!(record["reviewed_count"], 0);
    assert_eq!(record["accepted_count"], 0);
    assert_eq!(record["rejected_count"], 0);
    assert_eq!(record["skipped_count"], 1);
    assert_eq!(
        runs.find_run_record(&run_id)
            .await
            .unwrap()
            .expect("settled run")
            .status,
        AutomationRunStatus::Skipped
    );

    let admitted = application_run_ids(&runs).await;

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
    assert_eq!(
        route_refusal(&forbidden),
        application_invalid_request_error(UNKNOWN_FIELD_DETAIL),
        "{forbidden}"
    );

    assert_eq!(
        application_run_ids(&runs).await,
        admitted,
        "rejected curate calls must not append an application run"
    );

    fixture.harness.shutdown().await;
}

/// The run's settled ledger row, read the way callers read it: through
/// `tracedecay_automation_run_view`, which refuses an unsettled run as not
/// found.
async fn settled_run(server: &McpServer, run_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let response = handle_real_server_tool_call_raw(
            server,
            "tracedecay_automation_run_view",
            json!({"run_id": run_id, "format": "json"}),
        )
        .await;
        if response["error"].is_null() && response["result"]["isError"] != json!(true) {
            return tool_document(&response)["run"].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run {run_id} never settled: {response}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn tool_document(response: &Value) -> Value {
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("fact_store_curate content must be JSON")
}

async fn application_run_ids(runs: &RegisteredAutomationRunStore) -> BTreeSet<String> {
    let page = runs.load_run_records_page(50).await.unwrap();
    assert!(!page.has_more, "all fixture runs must fit on one page");
    page.records
        .into_iter()
        .filter(|record| record.trigger == AutomationTrigger::Application)
        .map(|record| record.run_id)
        .collect()
}
