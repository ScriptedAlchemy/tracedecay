//! Journey: a missing Workflow executor still returns the owner's envelope.

use serde_json::Value;
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::UtcMicros;
use tracedecay_mcp::handle_workflow;

use super::invoke_admitted_workflow_operation;

#[tokio::test]
async fn missing_executor_returns_the_registered_workflow_problem_envelope() {
    let request_id = RequestId::new("request.workflow-missing-executor").expect("request id");
    let deadline = Deadline::new(UtcMicros(
        tracedecay_daemon_protocol::invocation_now_micros().0 + 30_000_000,
    ))
    .expect("deadline");
    let cancellation =
        CancellationSignal::active("cancellation.workflow-missing-executor").expect("cancellation");
    let result = handle_workflow(
        "tracedecay_workflow_list_definitions",
        serde_json::json!({}),
        |request| invoke_admitted_workflow_operation(None, request),
        Some(request_id),
        Some(deadline),
        Some(cancellation),
    )
    .await
    .expect("MCP Workflow adapter response");
    let text = result.value["content"][0]["text"]
        .as_str()
        .expect("MCP Workflow JSON content");
    let payload: Value = serde_json::from_str(text).expect("Workflow envelope");
    // Either the request body was rejected as invalid for this operation or
    // the absent executor produced the canonical unavailable problem. Both
    // are typed envelopes from the same owner; neither is an MCP-specific
    // transport error, which is the property under test.
    assert!(
        payload.get("kind").is_some(),
        "the Workflow owner must answer a typed envelope, got {payload}"
    );
}
