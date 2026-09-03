//! MCP adapter for the canonical Work application owner.
//!
//! Work owns a typed HTTP envelope already. MCP invokes that exact owner and
//! returns its envelope as JSON content, so request decoding, binding lookup,
//! cancellation policy, result contracts, and failure taxonomy cannot drift
//! between the two transports.

use axum::body::to_bytes;
use serde_json::Value;
use tracedecay_api::{HttpApplicationControls, WorkHttpRequest, WorkOperation};
use tracedecay_application::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use tracedecay_application::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_daemon_protocol::{DaemonInvocationExecutor, invocation_now_micros};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::ToolResult;

use super::tool_call_support::json_result;

#[hotpath::measure(future = true, label = "mcp.work.total")]
pub(super) async fn handle_work(
    tool_name: &str,
    mut body: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult> {
    let (operation, request_id, controls) = hotpath::measure_block!("mcp.work.request_build", {
        let operation =
            work_operation_for_tool(tool_name).ok_or_else(|| TraceDecayError::Config {
                message: format!("unknown tool: {tool_name}"),
            })?;
        let request_id = protocol_request_id.map_or_else(mint_request_id, Ok)?;
        let controls = work_controls(
            operation,
            &request_id,
            protocol_deadline,
            protocol_cancellation,
        )?;
        if let Some(object) = body.as_object_mut() {
            // MCP presentation and request-correlation fields never belong to a
            // typed Work request body.
            object.remove("format");
            object.remove("__mcp_request_id");
        }
        (operation, request_id, controls)
    });
    let Some(executor) = executor else {
        return Err(TraceDecayError::project_route(
            "work.daemon_unavailable",
            true,
            "The Work daemon invocation owner is unavailable",
        ));
    };
    let response = hotpath::future!(
        crate::application_surface::invoke_work_operation(
            executor,
            WorkHttpRequest {
                operation,
                request_id,
                controls,
                body,
            },
        ),
        label = "mcp.work.invoke"
    )
    .await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "work.response_unavailable",
                true,
                format!("The Work application response could not be read: {error}"),
            )
        })?;
    hotpath::measure_block!("mcp.work.result_assemble", {
        let payload = serde_json::from_slice::<Value>(&body).map_err(|error| {
            TraceDecayError::project_route(
                "work.response_invalid",
                true,
                format!("The Work application response was not valid JSON: {error}"),
            )
        })?;
        let result = json_result(&payload);
        Ok(
            if payload.get("kind").and_then(Value::as_str) == Some("problem") {
                result.with_semantic_error(true)
            } else {
                result.with_semantic_error(false)
            },
        )
    })
}

pub(super) fn work_operation_for_tool(tool_name: &str) -> Option<WorkOperation> {
    let key = tool_name.strip_prefix("tracedecay_work_")?;
    WorkOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == key)
}

fn mint_request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate a Work request id".to_owned(),
    })
}

fn work_controls(
    operation: WorkOperation,
    request_id: &RequestId,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<HttpApplicationControls> {
    let operation_id = OperationId::new(operation.operation_id()).map_err(|error| {
        TraceDecayError::project_route(
            "work.operation_identity_unavailable",
            false,
            format!("The canonical Work operation identity is invalid: {error}"),
        )
    })?;
    let binding = tracedecay_application::work_executable_binding(&operation_id)
        .map_err(|error| {
            TraceDecayError::project_route(
                "work.catalog_unavailable",
                false,
                format!("The canonical Work catalog is unavailable: {error}"),
            )
        })?
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "work.binding_unavailable",
                false,
                "The canonical Work operation is not advertised by this build",
            )
        })?;
    let maximum_micros = i64::try_from(
        std::time::Duration::from_millis(binding.deadline().maximum_millis()).as_micros(),
    )
    .map_err(|_| {
        TraceDecayError::project_route(
            "work.deadline_unavailable",
            false,
            "The canonical Work deadline exceeds the domain clock",
        )
    })?;
    let maximum_deadline = UtcMicros(invocation_now_micros().0.saturating_add(maximum_micros));
    let deadline = protocol_deadline
        .filter(|deadline| deadline.expires_at <= maximum_deadline)
        .map_or_else(|| Deadline::new(maximum_deadline), Ok)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let cancellation = protocol_cancellation
        .map_or_else(
            || CancellationSignal::active(format!("cancellation.{}", request_id.as_str())),
            Ok,
        )
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    Ok(HttpApplicationControls {
        deadline,
        cancellation,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::body::to_bytes;
    use serde_json::{Value, json};
    use tracedecay_api::{HttpApplicationControls, WorkHttpRequest};
    use tracedecay_application::{
        ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
        ApplicationResponse, CancellationSignal, Deadline, InvocationError, RequestId,
    };
    use tracedecay_domain::UtcMicros;

    use super::{handle_work, work_operation_for_tool};

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
        ) -> ApplicationInvocationFuture<
            '_,
            std::result::Result<ApplicationResponse, InvocationError>,
        > {
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
            _event: tracedecay_application::feedback::observations::FeedbackSourceEventV1,
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

    #[test]
    fn maps_every_canonical_work_operation_without_a_second_name_list() {
        for operation in tracedecay_api::WorkOperation::ALL {
            let name = format!("tracedecay_work_{}", operation.operation_key());
            assert_eq!(work_operation_for_tool(&name), Some(operation));
        }
        assert_eq!(work_operation_for_tool("tracedecay_work_missing"), None);
        for retired in [
            "tracedecay_work_snapshot",
            "tracedecay_work_delta",
            "tracedecay_work_replan_dependencies",
            "tracedecay_work_accept_task",
        ] {
            assert_eq!(work_operation_for_tool(retired), None, "{retired}");
        }
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
            let request_id = RequestId::new(format!("request.work-mcp-parity-{index}"))
                .expect("valid request id");
            let deadline = Deadline::new(UtcMicros(
                tracedecay_daemon_protocol::invocation_now_micros().0 + 30_000_000,
            ))
            .expect("valid deadline");
            let cancellation =
                CancellationSignal::active(format!("cancellation.work-mcp-parity-{index}"))
                    .expect("valid cancellation signal");
            let http = response_json(
                crate::application_surface::invoke_work_operation(
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
                Some(&executor),
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
}
