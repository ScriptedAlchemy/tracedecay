//! MCP adapter for the canonical Work product invocation owner.

use serde_json::Value;
use tracedecay_api::WorkOperation;
use tracedecay_application::{
    CancellationSignal, Deadline, ExpandWorkEvidenceRequestV1, GenerateWorkProposalRequestV1,
    RequestId, WorkProductMutationRequestV1, WorkProductProjectionsRequestV1,
    WorkProductSnapshotRequestV1, WorkTaskEvidenceRequestV1,
};

use crate::daemon_client::{
    DaemonInvocationExecutor, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationRequest, WorkApplicationInvocationV1,
};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::ToolResult;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

use super::text_tool_result;

const DEFAULT_WORK_DEADLINE_MICROS: i64 = 30_000_000;

pub(super) async fn dispatch_product_tool(
    tool_name: &str,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult> {
    let Some(executor) = executor else {
        return Err(TraceDecayError::project_route(
            "work_product_daemon_unavailable",
            true,
            "the canonical Work product daemon owner is unavailable",
        ));
    };
    let (operation, request) = decode_request(tool_name, args)?;
    let request_id = match protocol_request_id {
        Some(request_id) => request_id,
        None => mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| {
            TraceDecayError::Config {
                message: "could not allocate a Work product request id".to_owned(),
            }
        })?,
    };
    let observed_at = invocation_now_micros();
    let deadline = match protocol_deadline {
        Some(deadline) => deadline,
        None => Deadline::new(tracedecay_domain::UtcMicros(
            observed_at.0.saturating_add(DEFAULT_WORK_DEADLINE_MICROS),
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
    let invocation = DaemonInvocationRequest::work_application(
        request_id.as_str(),
        request,
        observed_at,
        deadline.clone(),
        cancellation.context(),
    );
    let response = executor
        .invoke_controlled(
            invocation,
            deadline,
            cancellation,
            if operation.is_read_only() {
                InvocationCancellationPolicy::ReadOnly
            } else {
                InvocationCancellationPolicy::AuthoritativeEffect
            },
        )
        .await
        .map_err(|_| {
            TraceDecayError::project_route(
                "work_product_daemon_unavailable",
                true,
                "the canonical Work product daemon invocation did not complete",
            )
        })?;
    let semantic_error = matches!(
        &response.outcome,
        DaemonInvocationOutcome::ApplicationProblem { .. }
            | DaemonInvocationOutcome::Problem { .. }
    );
    let encoded = serde_json::to_string(&response)?;
    Ok(text_tool_result(&encoded).with_semantic_error(semantic_error))
}

fn decode_request(
    tool_name: &str,
    args: Value,
) -> Result<(WorkOperation, WorkApplicationInvocationV1)> {
    macro_rules! decode {
        ($operation:ident, $request:ty, $variant:ident) => {{
            let request = serde_json::from_value::<$request>(args).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("invalid {tool_name} request: {error}"),
                }
            })?;
            Ok((
                WorkOperation::$operation,
                WorkApplicationInvocationV1::$variant(request),
            ))
        }};
    }
    match tool_name {
        "tracedecay_product_snapshot" => decode!(
            ProductSnapshot,
            WorkProductSnapshotRequestV1,
            ProductSnapshot
        ),
        "tracedecay_product_projections" => decode!(
            ProductProjections,
            WorkProductProjectionsRequestV1,
            ProductProjections
        ),
        "tracedecay_task_evidence" => {
            decode!(TaskEvidence, WorkTaskEvidenceRequestV1, TaskEvidence)
        }
        "tracedecay_expand_task_evidence" => decode!(
            ExpandTaskEvidence,
            ExpandWorkEvidenceRequestV1,
            ExpandTaskEvidence
        ),
        "tracedecay_generate_work_proposal" => decode!(
            GenerateWorkProposal,
            GenerateWorkProposalRequestV1,
            GenerateWorkProposal
        ),
        "tracedecay_apply_work_command" => decode!(
            ApplyWorkCommand,
            WorkProductMutationRequestV1,
            ApplyWorkCommand
        ),
        _ => Err(super::unknown_tool_error(tool_name)),
    }
}
