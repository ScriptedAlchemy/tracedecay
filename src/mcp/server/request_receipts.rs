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
pub(super) enum ToolCallWorkerReconciliationStatus {
    Unavailable,
}

impl ToolCallWorkerReconciliationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ToolCallWorkerReconciliation {
    pub(super) id: u64,
    pub(super) status: ToolCallWorkerReconciliationStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolCallWorkerSettlement {
    NotStarted,
    Joined,
    Indeterminate(ToolCallWorkerReconciliation),
}

impl ToolCallWorkerSettlement {
    pub(super) const fn indeterminate(id: u64) -> Self {
        Self::Indeterminate(ToolCallWorkerReconciliation {
            id,
            status: ToolCallWorkerReconciliationStatus::Unavailable,
        })
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Joined => "joined",
            Self::Indeterminate(_) => "indeterminate",
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
        let mut receipt = json!({
            "route_admission_us": self.route_admission_us,
            "handler_us": self.handler_us,
            "result_materialization_us": self.result_materialization_us,
            "total_us": elapsed_micros(self.entered_at.elapsed()),
            "terminal": outcome.terminal.as_str(),
            "worker_settlement": outcome.worker_settlement.as_str(),
        });
        if let ToolCallWorkerSettlement::Indeterminate(reconciliation) = outcome.worker_settlement
            && let Some(receipt) = receipt.as_object_mut()
        {
            receipt.insert(
                "worker_reconciliation".to_owned(),
                json!({
                    "id": reconciliation.id,
                    "status": reconciliation.status.as_str(),
                }),
            );
        }
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

fn attach_execution_receipt(response: &mut JsonRpcResponse, receipt: Value) {
    if let Some(result) = response.result.as_mut() {
        let Some(result) = result.as_object_mut() else {
            tracing::error!("MCP tool result is not an object; execution receipt omitted");
            return;
        };
        let meta = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(EXECUTION_RECEIPT_KEY.to_owned(), receipt);
        } else {
            *meta = json!({ EXECUTION_RECEIPT_KEY: receipt });
        }
        return;
    }

    let Some(error) = response.error.as_mut() else {
        tracing::error!(
            "MCP tool response has neither result nor error; execution receipt omitted"
        );
        return;
    };
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
                    | "deadline_exceeded"
                    | "cancelled"
                    | "shutdown"
            )
        ));
        match receipt["worker_settlement"].as_str() {
            Some("not_started" | "joined") => {
                assert!(receipt.get("worker_reconciliation").is_none());
            }
            Some("indeterminate") => {
                let reconciliation = receipt
                    .get("worker_reconciliation")
                    .expect("indeterminate worker reconciliation");
                assert!(reconciliation["id"].as_u64().is_some_and(|id| id > 0));
                assert!(matches!(
                    reconciliation["status"].as_str(),
                    Some("pending" | "joined" | "failed" | "unavailable")
                ));
            }
            settlement => panic!("invalid worker settlement: {settlement:?}"),
        }
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
                JsonRpcResponse::error(json!(5), ErrorCode::InternalError, "deadline".to_owned()),
                ToolCallOutcome::new(
                    ToolCallTerminal::DeadlineExceeded,
                    ToolCallWorkerSettlement::indeterminate(7),
                ),
                "deadline_exceeded",
                "indeterminate",
            ),
            (
                JsonRpcResponse::error(json!(6), ErrorCode::InternalError, "cancelled".to_owned()),
                ToolCallOutcome::new(
                    ToolCallTerminal::Cancelled,
                    ToolCallWorkerSettlement::indeterminate(8),
                ),
                "cancelled",
                "indeterminate",
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
