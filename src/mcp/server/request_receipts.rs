use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::JsonRpcResponse;
use crate::errors::TraceDecayError;

const EXECUTION_RECEIPT_KEY: &str = "tracedecay/execution_receipt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolCallTerminal {
    Completed,
    Failed,
    Denied,
    Unavailable,
    Backpressured,
    DeadlineExceeded,
    Cancelled,
    Shutdown,
}

impl ToolCallTerminal {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::Backpressured => "backpressured",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Cancelled => "cancelled",
            Self::Shutdown => "shutdown",
        }
    }

    pub(super) fn for_error(error: &TraceDecayError) -> Self {
        let Some((reason_code, _, _)) = error.project_route_context() else {
            return Self::Failed;
        };
        match reason_code {
            "tool_dispatch_deadline_exceeded" => Self::DeadlineExceeded,
            "tool_dispatch_cancelled" | "request_cancelled" => Self::Cancelled,
            "tool_dispatch_shutdown" => Self::Shutdown,
            "tool_dispatch_saturated" => Self::Backpressured,
            "project_route_not_authorized"
            | "project_route_not_found"
            | "project_route_ambiguous" => Self::Denied,
            "project_route_unavailable"
            | "project_server_health_revoked"
            | "project_server_retired"
            | "mcp_dispatch_effect_journey_unverified"
            | "tool_unavailable" => Self::Unavailable,
            reason if reason.ends_with("_denied") || reason.ends_with("_not_authorized") => {
                Self::Denied
            }
            reason if reason.ends_with("_unavailable") => Self::Unavailable,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolCallWorkerSettlement {
    NotStarted,
    Settling,
    Joined,
}

impl ToolCallWorkerSettlement {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Settling => "settling",
            Self::Joined => "joined",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ToolCallOutcome {
    terminal: ToolCallTerminal,
    worker_settlement: ToolCallWorkerSettlement,
}

impl ToolCallOutcome {
    pub(super) const fn new(
        terminal: ToolCallTerminal,
        worker_settlement: ToolCallWorkerSettlement,
    ) -> Self {
        Self {
            terminal,
            worker_settlement,
        }
    }

    pub(super) const fn before_execution(terminal: ToolCallTerminal) -> Self {
        Self::new(terminal, ToolCallWorkerSettlement::NotStarted)
    }
}

/// Server-observed phases for one `tools/call` response.
///
/// The receipt is created before argument validation, so malformed,
/// unavailable, cancelled, and successful calls all use the same terminal
/// metadata authority.
#[derive(Debug)]
pub(super) struct ToolCallReceipt {
    entered_at: Instant,
    route_admission_us: u64,
    handler_us: u64,
    result_materialization_us: u64,
}

impl ToolCallReceipt {
    pub(super) fn new() -> Self {
        Self {
            entered_at: Instant::now(),
            route_admission_us: 0,
            handler_us: 0,
            result_materialization_us: 0,
        }
    }

    pub(super) fn set_route_admission(&mut self, elapsed: Duration) {
        self.route_admission_us = elapsed_micros(elapsed);
    }

    pub(super) fn set_handler(&mut self, elapsed: Duration) {
        self.handler_us = elapsed_micros(elapsed);
    }

    pub(super) fn set_result_materialization(&mut self, elapsed: Duration) {
        self.result_materialization_us = elapsed_micros(elapsed);
    }

    pub(super) fn finish(
        &self,
        mut response: JsonRpcResponse,
        outcome: ToolCallOutcome,
    ) -> JsonRpcResponse {
        let receipt = json!({
            "route_admission_us": self.route_admission_us,
            "handler_us": self.handler_us,
            "result_materialization_us": self.result_materialization_us,
            "total_us": elapsed_micros(self.entered_at.elapsed()),
            "terminal": outcome.terminal.as_str(),
            "worker_settlement": outcome.worker_settlement.as_str(),
        });
        attach_execution_receipt(&mut response, receipt);
        response
    }
}

pub(super) fn finish_early_tool_call_response(
    response: JsonRpcResponse,
    terminal: ToolCallTerminal,
) -> JsonRpcResponse {
    ToolCallReceipt::new().finish(response, ToolCallOutcome::before_execution(terminal))
}

fn elapsed_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn attach_execution_receipt(response: &mut JsonRpcResponse, mut receipt: Value) {
    if response.error.is_none()
        && let Some(result) = response.result.as_mut()
        && let Some(result) = result.as_object_mut()
    {
        let meta = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(EXECUTION_RECEIPT_KEY.to_owned(), receipt);
        } else {
            let original_meta = std::mem::take(meta);
            *meta = json!({
                "original_meta": original_meta,
                EXECUTION_RECEIPT_KEY: receipt,
            });
        }
        return;
    }

    if response.result.is_none()
        && let Some(error) = response.error.as_mut()
    {
        let data = error.data.get_or_insert_with(|| json!({}));
        if let Some(data) = data.as_object_mut() {
            data.insert(EXECUTION_RECEIPT_KEY.to_owned(), receipt);
            return;
        }
        let original_data = std::mem::take(data);
        *data = json!({
            "original_data": original_data,
            EXECUTION_RECEIPT_KEY: receipt,
        });
        return;
    }

    // A tools/call response must be exactly one object result or one error.
    // Convert every impossible shape into a typed JSON-RPC internal error and
    // keep the original payload in error data for diagnosis.
    receipt["terminal"] = json!(ToolCallTerminal::Failed.as_str());
    let original_result = response.result.take();
    let original_error = response.error.take();
    *response = JsonRpcResponse::error_with_data(
        response.id.clone(),
        crate::mcp::transport::ErrorCode::InternalError,
        "MCP tool response had an invalid result/error shape".to_owned(),
        Some(json!({
            "reason_code": "tool_response_invalid_shape",
            "retryable": false,
            "original_result": original_result,
            "original_error": original_error,
            EXECUTION_RECEIPT_KEY: receipt,
        })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::transport::ErrorCode;

    fn canonical_receipt(response: JsonRpcResponse) -> Value {
        let receipt = response
            .result
            .as_ref()
            .and_then(|result| result.get("_meta"))
            .and_then(|meta| meta.get(EXECUTION_RECEIPT_KEY))
            .or_else(|| {
                response
                    .error
                    .as_ref()
                    .and_then(|error| error.data.as_ref())
                    .and_then(|data| data.get(EXECUTION_RECEIPT_KEY))
            })
            .expect("execution receipt");
        assert!(receipt["total_us"].as_u64().is_some());
        assert!(matches!(
            receipt["terminal"].as_str(),
            Some(
                "completed"
                    | "failed"
                    | "denied"
                    | "unavailable"
                    | "backpressured"
                    | "deadline_exceeded"
                    | "cancelled"
                    | "shutdown"
            )
        ));
        assert!(matches!(
            receipt["worker_settlement"].as_str(),
            Some("not_started" | "settling" | "joined")
        ));
        receipt.clone()
    }

    #[test]
    fn canonical_receipt_distinguishes_all_terminal_and_worker_paths() {
        let timing = ToolCallReceipt::new();
        let cases = [
            (
                JsonRpcResponse::success(json!(1), json!({})),
                ToolCallOutcome::new(
                    ToolCallTerminal::Completed,
                    ToolCallWorkerSettlement::Joined,
                ),
                "completed",
                "joined",
            ),
            (
                JsonRpcResponse::error(
                    json!(2),
                    ErrorCode::InternalError,
                    "internal failure".to_owned(),
                ),
                ToolCallOutcome::new(ToolCallTerminal::Failed, ToolCallWorkerSettlement::Joined),
                "failed",
                "joined",
            ),
            (
                JsonRpcResponse::error(
                    json!(3),
                    ErrorCode::InvalidParams,
                    "scope denied".to_owned(),
                ),
                ToolCallOutcome::new(
                    ToolCallTerminal::Denied,
                    ToolCallWorkerSettlement::NotStarted,
                ),
                "denied",
                "not_started",
            ),
            (
                JsonRpcResponse::error(
                    json!(4),
                    ErrorCode::InternalError,
                    "authority unavailable".to_owned(),
                ),
                ToolCallOutcome::new(
                    ToolCallTerminal::Unavailable,
                    ToolCallWorkerSettlement::NotStarted,
                ),
                "unavailable",
                "not_started",
            ),
            (
                JsonRpcResponse::error(
                    json!("4b"),
                    ErrorCode::InternalError,
                    "dispatch capacity reached".to_owned(),
                ),
                ToolCallOutcome::new(
                    ToolCallTerminal::Backpressured,
                    ToolCallWorkerSettlement::NotStarted,
                ),
                "backpressured",
                "not_started",
            ),
            (
                JsonRpcResponse::error(json!(5), ErrorCode::InternalError, "deadline".to_owned()),
                ToolCallOutcome::new(
                    ToolCallTerminal::DeadlineExceeded,
                    ToolCallWorkerSettlement::Joined,
                ),
                "deadline_exceeded",
                "joined",
            ),
            (
                JsonRpcResponse::error(json!(6), ErrorCode::InternalError, "cancelled".to_owned()),
                ToolCallOutcome::new(
                    ToolCallTerminal::Cancelled,
                    ToolCallWorkerSettlement::Settling,
                ),
                "cancelled",
                "settling",
            ),
            (
                JsonRpcResponse::error(json!(7), ErrorCode::InternalError, "shutdown".to_owned()),
                ToolCallOutcome::new(
                    ToolCallTerminal::Shutdown,
                    ToolCallWorkerSettlement::NotStarted,
                ),
                "shutdown",
                "not_started",
            ),
        ];

        for (response, outcome, expected_terminal, expected_settlement) in cases {
            let receipt = canonical_receipt(timing.finish(response, outcome));
            assert_eq!(receipt["terminal"], expected_terminal);
            assert_eq!(receipt["worker_settlement"], expected_settlement);
        }
    }

    #[test]
    fn invalid_tool_response_shapes_become_receipted_internal_errors() {
        let timing = ToolCallReceipt::new();
        for response in [
            JsonRpcResponse::success(json!(8), json!("not an object")),
            JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: json!(9),
                result: None,
                error: None,
            },
            JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: json!(10),
                result: Some(json!({})),
                error: Some(crate::mcp::transport::JsonRpcError {
                    code: ErrorCode::InternalError.as_i32(),
                    message: "both branches".to_owned(),
                    data: None,
                }),
            },
        ] {
            let response = timing.finish(
                response,
                ToolCallOutcome::new(
                    ToolCallTerminal::Completed,
                    ToolCallWorkerSettlement::Joined,
                ),
            );
            assert!(response.result.is_none());
            let error = response.error.expect("typed internal error");
            assert_eq!(error.code, ErrorCode::InternalError.as_i32());
            assert_eq!(
                error
                    .data
                    .as_ref()
                    .and_then(|data| data["reason_code"].as_str()),
                Some("tool_response_invalid_shape")
            );
            assert_eq!(
                error
                    .data
                    .as_ref()
                    .and_then(|data| data[EXECUTION_RECEIPT_KEY]["terminal"].as_str()),
                Some("failed")
            );
        }
    }

    #[test]
    fn scalar_metadata_and_error_data_are_preserved_under_receipt_wrappers() {
        let success = ToolCallReceipt::new().finish(
            JsonRpcResponse::success(json!(11), json!({"_meta": "legacy"})),
            ToolCallOutcome::new(
                ToolCallTerminal::Completed,
                ToolCallWorkerSettlement::Joined,
            ),
        );
        assert_eq!(
            success.result.as_ref().expect("result")["_meta"]["original_meta"],
            "legacy"
        );
        canonical_receipt(success);

        let failure = ToolCallReceipt::new().finish(
            JsonRpcResponse::error_with_data(
                json!(12),
                ErrorCode::InternalError,
                "failed".to_owned(),
                Some(json!("legacy")),
            ),
            ToolCallOutcome::new(
                ToolCallTerminal::Failed,
                ToolCallWorkerSettlement::NotStarted,
            ),
        );
        assert_eq!(
            failure
                .error
                .as_ref()
                .expect("error")
                .data
                .as_ref()
                .expect("data")["original_data"],
            "legacy"
        );
        canonical_receipt(failure);
    }

    #[test]
    fn typed_failures_map_only_policy_scope_errors_to_denied() {
        let cases = [
            (
                TraceDecayError::Config {
                    message: "internal failure".to_owned(),
                },
                ToolCallTerminal::Failed,
            ),
            (
                TraceDecayError::project_route(
                    "project_route_not_authorized",
                    false,
                    "scope denied",
                ),
                ToolCallTerminal::Denied,
            ),
            (
                TraceDecayError::project_route(
                    "project_route_unavailable",
                    true,
                    "authority missing",
                ),
                ToolCallTerminal::Unavailable,
            ),
            (
                TraceDecayError::project_route(
                    "tool_dispatch_saturated",
                    true,
                    "retained dispatch capacity reached",
                ),
                ToolCallTerminal::Backpressured,
            ),
            (
                TraceDecayError::project_route("tool_dispatch_deadline_exceeded", true, "deadline"),
                ToolCallTerminal::DeadlineExceeded,
            ),
            (
                TraceDecayError::project_route("tool_dispatch_cancelled", true, "cancelled"),
                ToolCallTerminal::Cancelled,
            ),
            (
                TraceDecayError::project_route("tool_dispatch_shutdown", true, "shutdown"),
                ToolCallTerminal::Shutdown,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(ToolCallTerminal::for_error(&error), expected);
        }
    }
}
