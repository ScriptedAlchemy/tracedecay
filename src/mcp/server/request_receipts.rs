//! Canonical execution-receipt finalization for every MCP transport.

use serde_json::{Value, json};
use std::time::Duration;

use super::request_lifecycle::McpWorkerReconciliationReceipt;
use super::{McpRequestStart, McpToolDispatchControl, tool_errors::tool_error_response};
use crate::errors::TraceDecayError;
use crate::mcp::transport::{ErrorCode, JsonRpcResponse};

pub(super) const EXECUTION_RECEIPT_KEY: &str = "tracedecay/execution_receipt";
pub(crate) const APPLICATION_TERMINAL_KEY: &str = "tracedecay/application_terminal";

fn checked_elapsed_micros(elapsed: Duration) -> Option<u64> {
    u64::try_from(elapsed.as_micros()).ok()
}

pub(crate) fn is_project_retirement_reason_code(reason_code: Option<&str>) -> bool {
    matches!(
        reason_code,
        Some("project_server_health_revoked" | "project_server_retired")
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolCallTerminal {
    Completed,
    Failed,
    Denied,
    Unavailable,
    DeadlineExceeded,
    Cancelled,
    Shutdown,
}

impl McpToolCallTerminal {
    pub(crate) const fn as_str(self) -> &'static str {
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
}

pub(super) struct McpToolCallTiming {
    started: McpRequestStart,
}

impl McpToolCallTiming {
    pub(super) fn new(started: McpRequestStart) -> Self {
        Self { started }
    }

    fn receipt(
        &self,
        terminal: McpToolCallTerminal,
        control: Option<&McpToolDispatchControl>,
    ) -> Value {
        let (worker_settlement, worker_reconciliation) = control.map_or_else(
            || ("not_started", None),
            |control| {
                let (settlement, reconciliation) = control.worker_receipt_snapshot();
                (settlement.as_str(), reconciliation)
            },
        );
        self.receipt_with_worker_settlement(terminal, worker_settlement, worker_reconciliation)
    }

    fn receipt_with_worker_settlement(
        &self,
        terminal: McpToolCallTerminal,
        worker_settlement: &str,
        worker_reconciliation: Option<McpWorkerReconciliationReceipt>,
    ) -> Value {
        let elapsed_us = checked_elapsed_micros(self.started.elapsed());
        let mut receipt = json!({
            "total_us": elapsed_us,
            "terminal": terminal.as_str(),
            "worker_settlement": worker_settlement,
        });
        if elapsed_us.is_none()
            && let Some(object) = receipt.as_object_mut()
        {
            object.insert(
                "timing_state".to_owned(),
                Value::String("elapsed_microseconds_overflow".to_owned()),
            );
        }
        if let Some(reconciliation) = worker_reconciliation
            && let Some(object) = receipt.as_object_mut()
        {
            object.insert(
                "worker_reconciliation".to_owned(),
                json!({
                    "id": reconciliation.id,
                    "status": reconciliation.status.as_str(),
                }),
            );
        }
        receipt
    }
}

fn terminal_for_tool_response(response: &JsonRpcResponse) -> McpToolCallTerminal {
    let Some(error) = response.error.as_ref() else {
        if let Some(terminal) = response
            .result
            .as_ref()
            .and_then(|result| result.get("_meta"))
            .and_then(|meta| meta.get(APPLICATION_TERMINAL_KEY))
            .and_then(Value::as_str)
        {
            return match terminal {
                "cancelled" => McpToolCallTerminal::Cancelled,
                "deadline_exceeded" | "timed_out" => McpToolCallTerminal::DeadlineExceeded,
                "denied" => McpToolCallTerminal::Denied,
                "unavailable" => McpToolCallTerminal::Unavailable,
                "failed" | "effect_unknown" => McpToolCallTerminal::Failed,
                _ => McpToolCallTerminal::Completed,
            };
        }
        return if response
            .result
            .as_ref()
            .and_then(|result| result.get("isError"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            McpToolCallTerminal::Failed
        } else {
            McpToolCallTerminal::Completed
        };
    };
    let reason_code = error
        .data
        .as_ref()
        .and_then(|data| data.get("reason_code"))
        .and_then(Value::as_str);
    match reason_code {
        Some("tool_dispatch_deadline_exceeded") => McpToolCallTerminal::DeadlineExceeded,
        Some("tool_dispatch_cancelled" | "request_cancelled") => McpToolCallTerminal::Cancelled,
        Some("tool_dispatch_shutdown") => McpToolCallTerminal::Shutdown,
        Some(
            "catalog_binding_missing"
            | "catalog_binding_unavailable"
            | "daemon_draining"
            | "mcp_dispatch_effect_journey_unverified"
            | "mcp_dispatch_surface_not_mounted"
            | "message_search_unavailable"
            | "project_route_unavailable"
            | "project_server_health_revoked"
            | "project_server_retired"
            | "tool_dispatch_queue_saturated"
            | "tool_dispatch_reaper_saturated"
            | "tool_dispatch_reaper_unavailable"
            | "tool_unavailable",
        ) => McpToolCallTerminal::Unavailable,
        _ if error.code == ErrorCode::InvalidParams.as_i32() => McpToolCallTerminal::Denied,
        _ if error.code == ErrorCode::MethodNotFound.as_i32() => McpToolCallTerminal::Unavailable,
        _ => McpToolCallTerminal::Failed,
    }
}

fn normalize_tool_response(response: JsonRpcResponse) -> JsonRpcResponse {
    let valid_success =
        response.result.as_ref().is_some_and(Value::is_object) && response.error.is_none();
    let valid_error = response.result.is_none() && response.error.is_some();
    if valid_success || valid_error {
        return response;
    }
    JsonRpcResponse::error_with_data(
        response.id,
        ErrorCode::InternalError,
        "MCP tool response did not contain exactly one object result or error".to_owned(),
        Some(json!({
            "reason_code": "tool_response_materialization_invalid",
            "retryable": false,
        })),
    )
}

fn attach_execution_receipt(mut response: JsonRpcResponse, receipt: Value) -> JsonRpcResponse {
    if let Some(result) = response.result.as_mut()
        && let Some(result) = result.as_object_mut()
    {
        let meta = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(EXECUTION_RECEIPT_KEY.to_owned(), receipt);
        } else {
            *meta = json!({ EXECUTION_RECEIPT_KEY: receipt });
        }
        return response;
    }
    if let Some(error) = response.error.as_mut() {
        let data = error.data.get_or_insert_with(|| json!({}));
        if let Some(data) = data.as_object_mut() {
            data.insert(EXECUTION_RECEIPT_KEY.to_owned(), receipt);
        } else {
            let original_data = std::mem::take(data);
            *data = json!({
                "original_data": original_data,
                EXECUTION_RECEIPT_KEY: receipt,
            });
        }
    }
    response
}

pub(crate) fn finish_transport_cancelled_tool_call_response(
    id: Value,
    tool_name: &str,
    started: McpRequestStart,
    control: Option<&McpToolDispatchControl>,
) -> JsonRpcResponse {
    let mut response = JsonRpcResponse::error_with_data(
        id,
        ErrorCode::RequestCancelled,
        format!("tool '{tool_name}' was cancelled by the MCP client"),
        Some(json!({
            "tool": tool_name,
            "reason_code": "tool_dispatch_cancelled",
            "retryable": true,
        })),
    );
    response = attach_execution_receipt(
        response,
        McpToolCallTiming::new(started).receipt(McpToolCallTerminal::Cancelled, control),
    );
    response
}

pub(super) fn finish_tool_call_response(
    response: JsonRpcResponse,
    timing: &McpToolCallTiming,
    control: Option<&McpToolDispatchControl>,
    terminal: Option<McpToolCallTerminal>,
) -> JsonRpcResponse {
    let response = normalize_tool_response(response);
    let terminal = terminal.unwrap_or_else(|| terminal_for_tool_response(&response));
    attach_execution_receipt(response, timing.receipt(terminal, control))
}

pub(super) fn finish_early_tool_call_response(
    response: JsonRpcResponse,
    started: McpRequestStart,
) -> JsonRpcResponse {
    finish_tool_call_response(response, &McpToolCallTiming::new(started), None, None)
}

pub(super) fn finish_tool_error_response(
    id: Value,
    tool_name: &str,
    error: &TraceDecayError,
    started: McpRequestStart,
    control: Option<&McpToolDispatchControl>,
) -> JsonRpcResponse {
    finish_tool_call_response(
        tool_error_response(id, tool_name, error),
        &McpToolCallTiming::new(started),
        control,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_timing_overflow_is_explicit_instead_of_fabricated() {
        assert_eq!(checked_elapsed_micros(Duration::MAX), None);
    }

    #[test]
    fn early_failure_receipt_does_not_fabricate_a_worker_join() {
        let response = JsonRpcResponse::error(
            json!(1),
            ErrorCode::InvalidParams,
            "invalid request".to_owned(),
        );
        let response = finish_early_tool_call_response(response, McpRequestStart::now());
        let receipt = response
            .error
            .and_then(|error| error.data)
            .and_then(|data| data.get(EXECUTION_RECEIPT_KEY).cloned())
            .expect("execution receipt");
        assert_eq!(receipt["worker_settlement"], "not_started");
        assert_eq!(receipt["terminal"], "denied");
    }

    #[test]
    fn malformed_success_becomes_typed_terminal_error_with_receipt() {
        let response = JsonRpcResponse::success(json!(1), json!("not an object"));
        let response = finish_early_tool_call_response(response, McpRequestStart::now());
        let error = response.error.expect("typed terminal error");
        let data = error.data.expect("typed terminal data");
        assert_eq!(data["reason_code"], "tool_response_materialization_invalid");
        assert_eq!(data[EXECUTION_RECEIPT_KEY]["terminal"], "failed");
    }

    #[test]
    fn semantic_tool_error_is_a_failed_terminal() {
        let response = JsonRpcResponse::success(
            json!(1),
            json!({
                "content": [{"type": "text", "text": "tool failed"}],
                "isError": true,
            }),
        );
        let response = finish_early_tool_call_response(response, McpRequestStart::now());
        assert_eq!(
            response.result.expect("tool result")["_meta"][EXECUTION_RECEIPT_KEY]["terminal"],
            "failed"
        );
    }

    #[test]
    fn application_unavailable_metadata_is_an_unavailable_terminal() {
        let response = JsonRpcResponse::success(
            json!(1),
            json!({
                "content": [{"type": "text", "text": "temporarily unavailable"}],
                "isError": true,
                "_meta": {(APPLICATION_TERMINAL_KEY): "unavailable"},
            }),
        );
        let response = finish_early_tool_call_response(response, McpRequestStart::now());
        assert_eq!(
            response.result.expect("tool result")["_meta"][EXECUTION_RECEIPT_KEY]["terminal"],
            "unavailable"
        );
    }

    #[test]
    fn unverified_effect_journey_is_an_unavailable_terminal() {
        let response = JsonRpcResponse::error_with_data(
            json!(2),
            ErrorCode::InvalidParams,
            "effect journey is unavailable".to_owned(),
            Some(json!({
                "reason_code": "mcp_dispatch_effect_journey_unverified",
                "retryable": false,
            })),
        );
        let response = finish_early_tool_call_response(response, McpRequestStart::now());
        assert_eq!(
            response
                .error
                .expect("tool error")
                .data
                .expect("error data")[EXECUTION_RECEIPT_KEY]["terminal"],
            "unavailable"
        );
    }
}
