//! Canonical execution-receipt finalization for every MCP transport.

use serde_json::{Value, json};

use super::{McpRequestStart, McpToolDispatchControl, tool_errors::tool_error_response};
use crate::errors::TraceDecayError;
use crate::mcp::transport::{ErrorCode, JsonRpcResponse};

pub(super) const EXECUTION_RECEIPT_KEY: &str = "tracedecay/execution_receipt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum McpToolCallTerminal {
    Completed,
    Failed,
    Denied,
    Unavailable,
    DeadlineExceeded,
    Cancelled,
    Shutdown,
}

impl McpToolCallTerminal {
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
        let elapsed_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let (worker_settlement, worker_reconciliation) = control.map_or_else(
            || ("not_started", None),
            |control| {
                let (settlement, reconciliation) = control.worker_receipt_snapshot();
                (settlement.as_str(), reconciliation)
            },
        );
        let mut receipt = json!({
            "total_us": elapsed_us,
            "terminal": terminal.as_str(),
            "worker_settlement": worker_settlement,
        });
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
        return McpToolCallTerminal::Completed;
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
        tracing::error!("MCP tool response has no result or error; execution receipt omitted");
        return;
    };
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

pub(super) fn finish_tool_call_response(
    mut response: JsonRpcResponse,
    timing: &McpToolCallTiming,
    control: Option<&McpToolDispatchControl>,
    terminal: Option<McpToolCallTerminal>,
) -> JsonRpcResponse {
    let terminal = terminal.unwrap_or_else(|| terminal_for_tool_response(&response));
    attach_execution_receipt(&mut response, timing.receipt(terminal, control));
    response
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
}
