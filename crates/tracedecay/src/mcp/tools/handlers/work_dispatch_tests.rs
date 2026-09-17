//! Journey: MCP Work adapter preserves the typed HTTP envelope.

use std::sync::Mutex;

use axum::body::to_bytes;
use serde_json::{Value, json};
use tracedecay_api::{HttpApplicationControls, WorkHttpRequest};
use tracedecay_contracts::{
    ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
    ApplicationResponse, CancellationSignal, Deadline, InvocationError, RequestId,
};
use tracedecay_domain::UtcMicros;
use tracedecay_mcp::handle_work;
use tracedecay_mcp::handlers::work::work_operation_for_tool;

use super::invoke_admitted_work_operation;

/// Refuses the request after recording its closed route. Both transports
/// must preserve the identical typed Work unavailable envelope.
#[derive(Default)]
struct RecordingWorkExecutor {
    operations: Mutex<Vec<tracedecay_daemon_protocol::DaemonInvocationOperation>>,
}

impl ApplicationInvocationExecutor for RecordingWorkExecutor {
    fn invoke(
        &self,
        _invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, std::result::Result<ApplicationResponse, InvocationError>>
    {
        Box::pin(async { Err(InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for RecordingWorkExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        _deadline: Deadline,
        _cancellation: CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        self.operations
            .lock()
            .expect("recorded Work daemon operations")
            .push(request.operation());
        Box::pin(async { Err(tracedecay_daemon_protocol::DaemonInvocationError::Unavailable) })
    }

    fn observe_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: UtcMicros,
        _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        tracedecay_domain::errors::Result<()>,
    > {
        Box::pin(async { Ok(()) })
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("bounded Work HTTP response");
    serde_json::from_slice(&body).expect("JSON Work HTTP response")
}

fn mcp_json(result: tracedecay_mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .expect("MCP Work JSON content");
    serde_json::from_str(text).expect("MCP Work JSON envelope")
}

#[tokio::test]
async fn read_mutation_and_evidence_preserve_the_typed_http_work_envelope() {
    let executor = RecordingWorkExecutor::default();
    let requests = [
        (
            "tracedecay_work_views",
            json!({
                "selection": {"selection": "profile_owned_no_git"},
                "mode": {"mode": "current"},
                "continuation": null,
                "observed_at": 1
            }),
        ),
        ("tracedecay_work_resume_attempts", json!({"occurred_at": 1})),
        (
            "tracedecay_work_retrieve_evidence",
            json!({
                "selection": {"selection": "profile_owned_no_git"},
                "task_id": "task.work-mcp-parity",
                "verified_version": {
                    "graph_version": 1,
                    "event_sequence": 1,
                    "source_watermark": {},
                    "recovered_graph_digest": concat!(
                        "sha256:",
                        "11111111111111111111111111111111",
                        "11111111111111111111111111111111"
                    )
                },
                "temporal": {"kind": "forensic"},
                "page_size": 10,
                "expansion": null,
                "continuation": null,
                "observed_at": 1
            }),
        ),
    ];

    for (index, (tool_name, body)) in requests.into_iter().enumerate() {
        let operation = work_operation_for_tool(tool_name).expect("canonical Work name");
        let request_id =
            RequestId::new(format!("request.work-mcp-parity-{index}")).expect("valid request id");
        let deadline = Deadline::new(UtcMicros(
            tracedecay_daemon_protocol::invocation_now_micros().0 + 30_000_000,
        ))
        .expect("valid deadline");
        let cancellation =
            CancellationSignal::active(format!("cancellation.work-mcp-parity-{index}"))
                .expect("valid cancellation signal");
        let http = response_json(
            tracedecay_daemon_service::application_surface::invoke_work_operation(
                &executor,
                WorkHttpRequest {
                    operation,
                    request_id: request_id.clone(),
                    controls: HttpApplicationControls {
                        deadline: deadline.clone(),
                        cancellation: cancellation.clone(),
                    },
                    body: body.clone(),
                },
            )
            .await,
        )
        .await;
        let mcp = handle_work(
            tool_name,
            body,
            Some(|request| invoke_admitted_work_operation(&executor, request)),
            Some(request_id),
            Some(deadline),
            Some(cancellation),
        )
        .await
        .expect("MCP Work adapter response");

        assert_eq!(mcp.semantic_error(), Some(true));
        assert_eq!(
            mcp_json(mcp),
            http,
            "{tool_name} MCP envelope drifted from HTTP"
        );
    }

    assert_eq!(
        *executor
            .operations
            .lock()
            .expect("recorded Work daemon operations"),
        vec![
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
        ]
    );
}
