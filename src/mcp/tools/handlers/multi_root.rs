//! MCP adapter for the three daemon-owned multi-root operations.

use serde::Serialize;
use serde_json::{Value, json};
use tracedecay_application::multi_root::MultiRootApplicationOperation;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    CancellationSignal, Deadline, LegalAction, MultiRootExecuteRequestV1,
    MultiRootScopeSetCasRequestV1, MultiRootScopeSetReadRequestV1, ProblemOwningLayer, RequestId,
    ResultContractRef, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{BindingId, SchemaId};

use crate::daemon_client::{
    DaemonInvocationExecutor, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    DaemonInvocationResponse,
};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::ToolResult;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

use super::tool_call_support::json_result;

const DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;

pub(super) async fn handle_multi_root(
    tool_name: &str,
    body: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult> {
    let operation = operation_for_tool(tool_name).ok_or_else(|| TraceDecayError::Config {
        message: format!("unknown tool: {tool_name}"),
    })?;
    let request_id = match protocol_request_id {
        Some(request_id) => request_id,
        None => mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| {
            TraceDecayError::Config {
                message: "could not allocate a multi-root request id".to_owned(),
            }
        })?,
    };
    let observed_at = invocation_now_micros();
    let deadline = match protocol_deadline {
        Some(deadline) => deadline,
        None => Deadline::new(UtcMicros(
            observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS),
        ))
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
    };
    let cancellation = match protocol_cancellation {
        Some(cancellation) => cancellation,
        None => CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?,
    };
    // The MCP transport injects its protocol request id for cooperative
    // cancellation; the typed multi-root requests deny unknown fields and
    // never consume it (the protocol id already arrives as a parameter).
    let mut body = body;
    if let Some(map) = body.as_object_mut() {
        map.remove("__mcp_request_id");
    }
    let invocation = match operation {
        MultiRootApplicationOperation::ScopeSetRead => {
            let Ok(request) = serde_json::from_value::<MultiRootScopeSetReadRequestV1>(body) else {
                return invalid_request(operation, request_id);
            };
            DaemonInvocationRequest::multi_root_scope_set_read(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
        MultiRootApplicationOperation::ScopeSetCompareAndSwap => {
            let Ok(request) = serde_json::from_value::<MultiRootScopeSetCasRequestV1>(body) else {
                return invalid_request(operation, request_id);
            };
            DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
        MultiRootApplicationOperation::Execute => {
            let Ok(request) = serde_json::from_value::<MultiRootExecuteRequestV1>(body) else {
                return invalid_request(operation, request_id);
            };
            DaemonInvocationRequest::multi_root_execute(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
    };
    let policy = match operation {
        MultiRootApplicationOperation::ScopeSetCompareAndSwap => {
            InvocationCancellationPolicy::AuthoritativeEffect
        }
        MultiRootApplicationOperation::ScopeSetRead | MultiRootApplicationOperation::Execute => {
            InvocationCancellationPolicy::ReadOnly
        }
    };
    let Some(executor) = executor else {
        return problem_result(
            operation,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "multi_root.daemon_unavailable".to_owned(),
                message: "The multi-root daemon invocation owner is unavailable".to_owned(),
            }),
        );
    };
    let response = executor
        .invoke_controlled(invocation, deadline, cancellation, policy)
        .await;
    render_response(operation, request_id, response)
}

fn operation_for_tool(tool_name: &str) -> Option<MultiRootApplicationOperation> {
    match tool_name {
        "tracedecay_multi_root_scope_set_read" => Some(MultiRootApplicationOperation::ScopeSetRead),
        "tracedecay_multi_root_scope_set_compare_and_swap" => {
            Some(MultiRootApplicationOperation::ScopeSetCompareAndSwap)
        }
        "tracedecay_multi_root_execute" => Some(MultiRootApplicationOperation::Execute),
        _ => None,
    }
}

fn render_response(
    operation: MultiRootApplicationOperation,
    request_id: RequestId,
    response: std::result::Result<
        DaemonInvocationResponse,
        crate::daemon_client::DaemonInvocationError,
    >,
) -> Result<ToolResult> {
    match response {
        Ok(DaemonInvocationResponse { outcome, .. }) => match outcome {
            DaemonInvocationOutcome::MultiRootScopeSetRead { scope, outcome }
                if operation == MultiRootApplicationOperation::ScopeSetRead =>
            {
                success_result(operation, request_id, scope, outcome)
            }
            DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { scope, outcome }
                if operation == MultiRootApplicationOperation::ScopeSetCompareAndSwap =>
            {
                success_result(operation, request_id, scope, outcome)
            }
            DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome }
                if operation == MultiRootApplicationOperation::Execute =>
            {
                success_result(operation, request_id, scope, outcome)
            }
            DaemonInvocationOutcome::ApplicationProblem { problem } => {
                problem_result(operation, request_id, problem)
            }
            DaemonInvocationOutcome::Problem { problem } => {
                problem_result(operation, request_id, daemon_problem(problem))
            }
            _ => problem_result(
                operation,
                request_id,
                unavailable("multi_root.protocol_unavailable"),
            ),
        },
        Err(error) => problem_result(operation, request_id, error.into_application_problem()),
    }
}

fn success_result<T>(
    operation: MultiRootApplicationOperation,
    request_id: RequestId,
    scope: tracedecay_application::ResolvedScope,
    outcome: ApplicationOutcome<T>,
) -> Result<ToolResult>
where
    T: Serialize,
{
    let envelope = ApplicationEnvelope {
        contract: result_contract(operation)?,
        request_id,
        scope,
        outcome,
    };
    let payload = json!({
        "binding_id": binding_id(operation)?,
        "application": envelope,
    });
    Ok(json_result(&payload))
}

fn invalid_request(
    operation: MultiRootApplicationOperation,
    request_id: RequestId,
) -> Result<ToolResult> {
    problem_result(
        operation,
        request_id,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "multi_root.invalid_request".to_owned(),
                message: "The multi-root application request is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
    )
}

fn problem_result(
    operation: MultiRootApplicationOperation,
    request_id: RequestId,
    problem: ApplicationProblem,
) -> Result<ToolResult> {
    let payload = json!({
        "binding_id": binding_id(operation)?,
        "application": ApplicationProblemEnvelope::new(result_contract(operation)?, request_id, problem)
            .with_owning_layer(ProblemOwningLayer::Runtime),
    });
    Ok(json_result(&payload).with_semantic_error(true))
}

fn daemon_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "multi_root.invalid_request".to_owned(),
                    message: "The multi-root application request is invalid".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            }
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        // The store the scope set lives in refused its own shape. Retrying the
        // same request cannot help until the operator resets it, so this is a
        // request-correcting problem rather than a retryable outage.
        DaemonInvocationProblem::ResetRequired => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "multi_root.reset_required".to_owned(),
                message: "The multi-root scope-set store requires an explicit reset".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        DaemonInvocationProblem::Unavailable => unavailable("multi_root.unavailable"),
    }
}

fn unavailable(code: &'static str) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: code.to_owned(),
        message: "The multi-root application runtime is unavailable".to_owned(),
    })
}

fn binding_id(operation: MultiRootApplicationOperation) -> Result<BindingId> {
    BindingId::new(format!(
        "binding.http.multi_root.{}.v1",
        operation.operation_key()
    ))
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })
}

fn result_contract(operation: MultiRootApplicationOperation) -> Result<ResultContractRef> {
    let suffix = match operation {
        MultiRootApplicationOperation::ScopeSetRead => "scope-set-read",
        MultiRootApplicationOperation::ScopeSetCompareAndSwap => "scope-set-compare-and-swap",
        MultiRootApplicationOperation::Execute => "execute",
    };
    let schema = SchemaId::new(format!("schema.tracedecay.multi-root.{suffix}-result.v1"))
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    ResultContractRef::new(schema, 1).map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::handle_multi_root;

    fn problem_code(result: &crate::mcp::tools::ToolResult) -> String {
        let text = result.value["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        payload["application"]["problem"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn transport_request_id_is_stripped_before_typed_deserialization() {
        let body = json!({
            "scope_set_id": "tool-sweep-scope-set.v1",
            "__mcp_request_id": "request.mcp.fixture",
        });

        let result = handle_multi_root(
            "tracedecay_multi_root_scope_set_read",
            body,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // The injected transport id must not fail typed deserialization; the
        // request reaches the daemon-owner gate and reports its absence.
        assert_eq!(problem_code(&result), "multi_root.daemon_unavailable");
    }

    #[tokio::test]
    async fn genuinely_unknown_fields_still_reject_as_invalid_request() {
        let body = json!({
            "scope_set_id": "tool-sweep-scope-set.v1",
            "unexpected_field": true,
        });

        let result = handle_multi_root(
            "tracedecay_multi_root_scope_set_read",
            body,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(problem_code(&result), "multi_root.invalid_request");
    }
}
