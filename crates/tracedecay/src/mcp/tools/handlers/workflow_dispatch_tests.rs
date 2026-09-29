//! Journey: a missing Workflow executor still returns the owner's envelope.

use serde_json::Value;
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::UtcMicros;
use tracedecay_mcp::handle_workflow;

use super::invoke_admitted_workflow_operation;

#[tokio::test]
async fn missing_executor_returns_the_registered_workflow_problem_envelope() {
    let request_id = RequestId::new("request.workflow-missing-executor").expect("request id");
    let deadline = Deadline::new(UtcMicros(tracedecay_contracts::now_micros().0 + 30_000_000))
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
    // The absent executor answers the owner's canonical unavailable problem,
    // not an MCP-specific transport error.
    assert_eq!(payload["kind"], "problem", "{payload}");
    let problem = &payload["value"]["problem"];
    assert_eq!(
        problem["code"], "workflow.transport_unavailable",
        "{payload}"
    );
    assert_eq!(problem["kind"], "unavailable", "{payload}");
    assert_eq!(
        payload["value"]["binding_id"],
        "binding.http.workflow.list_definitions"
    );
    assert_eq!(
        payload["value"]["request_id"],
        "request.workflow-missing-executor"
    );
}
