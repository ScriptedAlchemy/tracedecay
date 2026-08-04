use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;

use super::recover_lock;
use crate::errors::Result;
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

pub(crate) struct DispatchExecutionSettlement {
    state: std::sync::atomic::AtomicU8,
}

impl DispatchExecutionSettlement {
    const STARTED: u8 = 1;
    const JOINED: u8 = 2;

    pub(crate) fn new() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
        }
    }

    pub(crate) async fn observe<T, F>(self: Arc<Self>, future: F) -> T
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
            Self::STARTED => ToolCallWorkerSettlement::Settling,
            _ => ToolCallWorkerSettlement::NotStarted,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_settling(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::STARTED
    }

    #[cfg(test)]
    pub(crate) fn is_joined(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::JOINED
    }
}

/// Server-owned tasks whose handlers outlive the transport-visible deadline.
///
/// A timed-out request gets a bounded response while its admitted handler
/// remains owned here. Completed tasks are reaped on later admissions and all
/// remaining tasks are joined during server shutdown.
pub(crate) struct RetainedToolDispatchTasks {
    accepting: std::sync::atomic::AtomicBool,
    tasks: std::sync::Mutex<tokio::task::JoinSet<()>>,
}

impl RetainedToolDispatchTasks {
    pub(crate) fn new() -> Self {
        Self {
            accepting: std::sync::atomic::AtomicBool::new(true),
            tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
        }
    }

    pub(crate) fn spawn<T, F>(&self, future: F) -> Result<tokio::sync::oneshot::Receiver<T>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let mut tasks = recover_lock(&self.tasks);
        while let Some(joined) = tasks.try_join_next() {
            if let Err(error) = joined {
                tracing::error!(error = %error, "retained MCP tool dispatch task failed");
            }
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(crate::errors::TraceDecayError::project_route(
                "tool_dispatch_shutdown",
                true,
                "MCP server is shutting down and cannot retain another tool dispatch",
            ));
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let output = future.await;
            let _ = sender.send(output);
        });
        Ok(receiver)
    }

    pub(crate) async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let mut tasks = {
            let mut retained = recover_lock(&self.tasks);
            std::mem::take(&mut *retained)
        };
        while let Some(joined) = tasks.join_next().await {
            if let Err(error) = joined {
                tracing::error!(error = %error, "retained MCP tool dispatch task failed");
            }
        }
    }
}
