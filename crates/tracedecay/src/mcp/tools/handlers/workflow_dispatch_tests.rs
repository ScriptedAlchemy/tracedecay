//! Journey: a missing Workflow executor still returns the owner's envelope.

use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::{Value, json};
use tracedecay_api::{HttpApplicationControls, WorkflowHttpRequest, WorkflowOperation};
use tracedecay_contracts::{
    ApplicationOutcome, CancellationSignal, Deadline, RequestId, ResolvedScope,
};
use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkflowApplicationOutcome};
use tracedecay_domain::{ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_mcp::handle_workflow;

use super::dispatch_test_support::tool_json;
use super::invoke_admitted_workflow_operation;
use super::work_dispatch_tests::RecordingApplicationExecutor;

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
    let payload = tool_json(&result);
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

#[tokio::test]
async fn workflow_read_preserves_results_and_refuses_wrong_operations_and_fields() {
    let scope = ResolvedScope::new(
        ProjectId::new("project.workflow-decoding").unwrap(),
        RepositoryId::new("repository.workflow-decoding").unwrap(),
        WorktreeId::new("worktree.workflow-decoding").unwrap(),
        None,
    )
    .unwrap();
    for (outcome, body, expected_problem, invocations) in [
        (
            WorkflowApplicationOutcome::ListDefinitions(ApplicationOutcome::Result(Vec::new())),
            json!({}),
            None,
            2,
        ),
        (
            // The payload DTO is identical to ListDefinitions, but it belongs
            // to another operation and must never pass the result selector.
            WorkflowApplicationOutcome::DefinitionHistory(ApplicationOutcome::Result(Vec::new())),
            json!({}),
            Some("workflow.protocol_unavailable"),
            2,
        ),
        (
            WorkflowApplicationOutcome::ListDefinitions(ApplicationOutcome::Result(Vec::new())),
            json!({"unexpected": true}),
            Some("workflow.invalid_request"),
            0,
        ),
    ] {
        let executor = RecordingApplicationExecutor::with_settlement(
            DaemonInvocationOutcome::WorkflowApplication {
                scope: scope.clone(),
                outcome,
            },
        );
        let request_id = RequestId::new("request.workflow-decoding").unwrap();
        let controls = HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(tracedecay_contracts::now_micros().0 + 30_000_000))
                .unwrap(),
            cancellation: CancellationSignal::active("cancellation.workflow-decoding").unwrap(),
        };
        let http = tracedecay_daemon_service::application_surface::invoke_workflow_operation(
            Some(&executor),
            WorkflowHttpRequest {
                operation: WorkflowOperation::ListDefinitions,
                request_id: request_id.clone(),
                controls: controls.clone(),
                body: body.clone(),
            },
        )
        .await
        .expect("typed Workflow owner response")
        .into_response();
        let http: Value =
            serde_json::from_slice(&to_bytes(http.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        let mcp = handle_workflow(
            "tracedecay_workflow_list_definitions",
            body,
            |request| invoke_admitted_workflow_operation(Some(&executor), request),
            Some(request_id.clone()),
            Some(controls.deadline),
            Some(controls.cancellation),
        )
        .await
        .expect("MCP Workflow result");
        assert_eq!(tool_json(&mcp), http);
        assert_eq!(http["value"]["request_id"], request_id.as_str());
        assert_eq!(executor.invocation_count(), invocations);
        assert_eq!(mcp.semantic_error(), Some(expected_problem.is_some()));
        if let Some(code) = expected_problem {
            assert_eq!(http["value"]["problem"]["code"], code);
        } else {
            assert_eq!(http["kind"], "success");
            assert_eq!(
                http["value"]["outcome"],
                json!({"outcome": "result", "value": []})
            );
            assert_eq!(
                http["value"]["scope"],
                serde_json::to_value(&scope).unwrap()
            );
        }
    }
}
