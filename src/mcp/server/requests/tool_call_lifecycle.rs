use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::recover_lock;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::ProjectRegistryContext;
use crate::mcp::ToolResult;
use crate::mcp::project_route::ResolvedProjectRoute;
use crate::mcp::server::request_receipts::{ToolCallTerminal, ToolCallWorkerSettlement};
use crate::mcp::transport::JsonRpcResponse;
use crate::tracedecay::TraceDecay;

pub(in crate::mcp::server) struct ApplicationCancellationRegistration<'a> {
    pub(super) registry:
        &'a std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
    pub(super) request_id: Option<String>,
}

impl Drop for ApplicationCancellationRegistration<'_> {
    fn drop(&mut self) {
        if let Some(request_id) = self.request_id.as_deref() {
            recover_lock(self.registry).remove(request_id);
        }
    }
}

/// Retained name for server call sites; the saturating clamp is the shared
/// definition so MCP cannot stamp "now" differently from the daemon.
pub(in crate::mcp::server) fn mcp_now_micros() -> tracedecay_domain::UtcMicros {
    tracedecay_application::clock::now_micros()
}

pub(in crate::mcp::server) struct PreparedToolCall<'a> {
    pub(super) tool_name: String,
    pub(super) arguments: Value,
    pub(super) analytics_arguments: Value,
    pub(super) analytics_session_id: Option<String>,
    pub(super) dispatch_control: crate::mcp::tools::handlers::McpToolDispatchControl,
    pub(super) application_request_id: Option<tracedecay_application::RequestId>,
    pub(super) _cancellation_registration: ApplicationCancellationRegistration<'a>,
}

pub(in crate::mcp::server) struct PreparedToolCallError {
    pub(super) response: JsonRpcResponse,
    pub(super) terminal: ToolCallTerminal,
}

pub(in crate::mcp::server) struct DispatchedToolCall {
    pub(super) cg: Arc<TraceDecay>,
    pub(super) selected_owner: Option<ProjectRegistryContext>,
    pub(super) selected_scope: Option<tracedecay_application::ResolvedScope>,
    pub(super) outcome: Result<ToolResult>,
    pub(super) elapsed_us: Option<u64>,
    pub(super) worker_settlement: ToolCallWorkerSettlement,
}

pub(in crate::mcp::server) struct RoutedToolCall {
    pub(super) arguments: Value,
    pub(super) selected_project: Option<ResolvedProjectRoute>,
}

pub(in crate::mcp::server) struct ToolTokenAccounting {
    pub(super) raw_file_tokens: u64,
    pub(super) response_tokens: u64,
    pub(super) net_saved_tokens: u64,
}

static NEXT_DISPATCH_EXECUTION_RECONCILIATION_ID: AtomicU64 = AtomicU64::new(1);

pub(in crate::mcp::server) struct DispatchExecutionSettlement {
    reconciliation_id: u64,
    state: std::sync::atomic::AtomicU8,
}

impl DispatchExecutionSettlement {
    const STARTED: u8 = 1;
    const JOINED: u8 = 2;

    pub(super) fn new() -> Result<Self> {
        let reconciliation_id = NEXT_DISPATCH_EXECUTION_RECONCILIATION_ID
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .map_err(|_| TraceDecayError::Config {
                message: "MCP dispatch execution reconciliation identity exhausted".to_owned(),
            })?;
        Ok(Self {
            reconciliation_id,
            state: std::sync::atomic::AtomicU8::new(0),
        })
    }

    pub(super) async fn observe<T, F>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.state.store(Self::STARTED, Ordering::Release);
        let output = future.await;
        self.state.store(Self::JOINED, Ordering::Release);
        output
    }

    pub(super) fn snapshot(&self) -> ToolCallWorkerSettlement {
        match self.state.load(Ordering::Acquire) {
            Self::JOINED => ToolCallWorkerSettlement::Joined,
            Self::STARTED => ToolCallWorkerSettlement::indeterminate(self.reconciliation_id),
            _ => ToolCallWorkerSettlement::NotStarted,
        }
    }
}
