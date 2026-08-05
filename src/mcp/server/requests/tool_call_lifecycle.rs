use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

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

impl crate::mcp::server::McpServer {
    #[allow(clippy::result_large_err)]
    pub(super) fn prepare_tool_call<'a>(
        &'a self,
        id: &Value,
        params: Option<&Value>,
        memory_request_scope: &str,
        pre_cancelled: bool,
    ) -> std::result::Result<PreparedToolCall<'a>, PreparedToolCallError> {
        let Some(params) = params else {
            return Err(PreparedToolCallError {
                response: JsonRpcResponse::error(
                    id.clone(),
                    crate::mcp::transport::ErrorCode::InvalidParams,
                    "missing params for tools/call".to_string(),
                ),
                terminal: ToolCallTerminal::Failed,
            });
        };

        let Some(tool_name) = params.get("name").and_then(Value::as_str) else {
            return Err(PreparedToolCallError {
                response: JsonRpcResponse::error(
                    id.clone(),
                    crate::mcp::transport::ErrorCode::InvalidParams,
                    "missing 'name' in tools/call params".to_string(),
                ),
                terminal: ToolCallTerminal::Failed,
            });
        };

        let application_request_id =
            super::application_surface_request_id(id, memory_request_scope)
                .and_then(|request_id| tracedecay_application::RequestId::new(request_id).ok());
        let cancellation_identity = application_request_id.as_ref().map_or_else(
            || format!("cancellation.mcp.unidentified.{tool_name}"),
            |request_id| format!("cancellation.{}", request_id.as_str()),
        );
        let cancellation = tracedecay_application::CancellationSignal::active(
            cancellation_identity,
        )
        .map_err(|error| {
            let error = TraceDecayError::Config {
                message: format!("could not create MCP cancellation signal: {error}"),
            };
            PreparedToolCallError {
                response: super::tool_error_response(id.clone(), tool_name, &error),
                terminal: ToolCallTerminal::for_error(&error),
            }
        })?;
        if pre_cancelled {
            cancellation.cancel(mcp_now_micros());
        }
        if let Some(request_id) = application_request_id.as_ref() {
            recover_lock(&self.application_surface_cancellations)
                .insert(request_id.as_str().to_owned(), cancellation.clone());
        }
        let registration = ApplicationCancellationRegistration {
            registry: &self.application_surface_cancellations,
            request_id: application_request_id
                .as_ref()
                .map(|request_id| request_id.as_str().to_owned()),
        };
        let application_surface =
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
        let source_edit = super::is_source_edit_tool(tool_name);
        let controlled_read = super::is_controlled_read_tool(tool_name);
        let carried_deadline = super::dispatch_deadline_horizon_micros(
            application_surface.is_some() || source_edit,
            controlled_read || source_edit,
        )
        .and_then(|horizon| {
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                mcp_now_micros().0.saturating_add(horizon),
            ))
            .ok()
        });
        let dispatch_control = crate::mcp::tools::handlers::McpToolDispatchControl::new(
            tool_name,
            carried_deadline,
            cancellation,
        )
        .map_err(|error| PreparedToolCallError {
            response: super::tool_error_response(id.clone(), tool_name, &error),
            terminal: ToolCallTerminal::for_error(&error),
        })?;
        dispatch_control
            .check(crate::mcp::tools::handlers::McpToolDispatchStage::SchemaValidation)
            .map_err(|error| PreparedToolCallError {
                response: super::tool_error_response(id.clone(), tool_name, &error),
                terminal: ToolCallTerminal::for_error(&error),
            })?;

        let mut arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if crate::mcp::project_route::protect_tool_structural_ids(&mut arguments).is_err() {
            return Err(PreparedToolCallError {
                response: JsonRpcResponse::error(
                    id.clone(),
                    crate::mcp::transport::ErrorCode::InvalidParams,
                    "invalid structural identifier".to_string(),
                ),
                terminal: ToolCallTerminal::Failed,
            });
        }

        Ok(PreparedToolCall {
            tool_name: tool_name.to_string(),
            analytics_arguments: arguments.clone(),
            analytics_session_id: super::mcp_analytics_session_id(&arguments),
            arguments,
            dispatch_control,
            application_request_id,
            _cancellation_registration: registration,
        })
    }
}

pub(in crate::mcp::server) struct DispatchedToolCall {
    pub(super) cg: Arc<TraceDecay>,
    pub(super) selected_owner: Option<ProjectRegistryContext>,
    pub(super) selected_scope: Option<tracedecay_application::ResolvedScope>,
    pub(super) outcome: Result<ToolResult>,
    pub(super) elapsed_us: Option<u64>,
    pub(super) worker_settlement: ToolCallWorkerSettlement,
    pub(super) settlement: Arc<DispatchExecutionSettlement>,
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

    pub(crate) fn is_joined(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::JOINED
    }
}

/// Server-owned tasks whose handlers outlive the transport-visible deadline.
///
/// A timed-out request gets a bounded response while its admitted handler
/// remains owned here. Each active task retains this registry, and its join-set
/// output retains the settlement receipt until reconciliation reaps the joined
/// task. Shutdown closes admission and waits only for its bounded grace;
/// non-cooperative work keeps the owner alive until it actually joins.
pub(crate) struct RetainedToolDispatchTasks {
    accepting: std::sync::atomic::AtomicBool,
    policy: RetainedToolDispatchPolicy,
    state: tokio::sync::Mutex<RetainedToolDispatchState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedToolDispatchLane {
    General,
    Control,
}

#[derive(Clone, Copy)]
struct RetainedToolDispatchPolicy {
    general_capacity: usize,
    reserved_control_capacity: usize,
    shutdown_grace: std::time::Duration,
}

impl RetainedToolDispatchPolicy {
    fn for_host() -> Self {
        let parallelism = std::thread::available_parallelism().map_or(4, usize::from);
        Self {
            general_capacity: parallelism.saturating_mul(8).clamp(16, 256),
            reserved_control_capacity: parallelism.clamp(2, 8),
            shutdown_grace: std::time::Duration::from_millis(100),
        }
    }

    #[cfg(test)]
    fn fixture(general_capacity: usize, reserved_control_capacity: usize) -> Self {
        Self {
            general_capacity,
            reserved_control_capacity,
            shutdown_grace: std::time::Duration::from_millis(100),
        }
    }

    fn total_capacity(self) -> usize {
        self.general_capacity
            .saturating_add(self.reserved_control_capacity)
    }
}

struct RetainedToolDispatchState {
    tasks: tokio::task::JoinSet<Arc<DispatchExecutionSettlement>>,
    lanes: std::collections::HashMap<tokio::task::Id, RetainedToolDispatchLane>,
}

impl RetainedToolDispatchTasks {
    pub(crate) fn new() -> Self {
        Self::with_policy(RetainedToolDispatchPolicy::for_host())
    }

    fn with_policy(policy: RetainedToolDispatchPolicy) -> Self {
        Self {
            accepting: std::sync::atomic::AtomicBool::new(true),
            policy,
            state: tokio::sync::Mutex::new(RetainedToolDispatchState {
                tasks: tokio::task::JoinSet::new(),
                lanes: std::collections::HashMap::new(),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(general_capacity: usize, reserved_control_capacity: usize) -> Self {
        Self::with_policy(RetainedToolDispatchPolicy::fixture(
            general_capacity,
            reserved_control_capacity,
        ))
    }

    pub(crate) async fn spawn<T, F>(
        self: &Arc<Self>,
        lane: RetainedToolDispatchLane,
        settlement: Arc<DispatchExecutionSettlement>,
        future: F,
    ) -> Result<tokio::sync::oneshot::Receiver<T>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let mut state = self.state.lock().await;
        Self::reap_finished(&mut state);
        if !self.accepting.load(Ordering::Acquire) {
            return Err(crate::errors::TraceDecayError::project_route(
                "tool_dispatch_shutdown",
                true,
                "MCP server is shutting down and cannot retain another tool dispatch",
            ));
        }
        let general_active = state
            .lanes
            .values()
            .filter(|active| **active == RetainedToolDispatchLane::General)
            .count();
        let saturated = match lane {
            RetainedToolDispatchLane::General => general_active >= self.policy.general_capacity,
            RetainedToolDispatchLane::Control => state.lanes.len() >= self.policy.total_capacity(),
        };
        if saturated {
            return Err(crate::errors::TraceDecayError::project_route(
                "tool_dispatch_saturated",
                true,
                format!(
                    "MCP retained dispatch {:?} lane is backpressured ({} general + {} reserved control slots)",
                    lane, self.policy.general_capacity, self.policy.reserved_control_capacity,
                ),
            ));
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task_owner = Arc::clone(self);
        let task = state.tasks.spawn(async move {
            let output = Arc::clone(&settlement).observe(future).await;
            let _ = sender.send(output);
            drop(task_owner);
            settlement
        });
        state.lanes.insert(task.id(), lane);
        Ok(receiver)
    }

    fn reap_finished(state: &mut RetainedToolDispatchState) {
        while let Some(joined) = state.tasks.try_join_next_with_id() {
            match joined {
                Ok((id, settlement)) => {
                    state.lanes.remove(&id);
                    debug_assert!(settlement.is_joined());
                }
                Err(error) => {
                    state.lanes.remove(&error.id());
                    tracing::error!(error = %error, "retained MCP tool dispatch task failed");
                }
            }
        }
    }

    pub(crate) async fn shutdown(&self) -> bool {
        self.shutdown_within(self.policy.shutdown_grace).await
    }

    pub(crate) async fn shutdown_within(&self, timeout: std::time::Duration) -> bool {
        let state = self.state.lock().await;
        // Holding the task-set lock serializes closure with spawn's admission
        // check, so no task can publish after shutdown reports drainage.
        self.accepting.store(false, Ordering::Release);
        self.reconcile_locked_within(state, timeout).await
    }

    #[cfg(test)]
    pub(crate) async fn reconcile_within(&self, timeout: std::time::Duration) -> bool {
        let state = self.state.lock().await;
        self.reconcile_locked_within(state, timeout).await
    }

    async fn reconcile_locked_within(
        &self,
        mut state: tokio::sync::MutexGuard<'_, RetainedToolDispatchState>,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            Self::reap_finished(&mut state);
            if state.tasks.is_empty() {
                return true;
            }
            match tokio::time::timeout_at(deadline, state.tasks.join_next_with_id()).await {
                Ok(Some(Ok((id, settlement)))) => {
                    state.lanes.remove(&id);
                    debug_assert!(settlement.is_joined());
                }
                Ok(Some(Err(error))) => {
                    state.lanes.remove(&error.id());
                    tracing::error!(error = %error, "retained MCP tool dispatch task failed");
                }
                Ok(None) => return true,
                Err(_) => {
                    Self::reap_finished(&mut state);
                    return state.tasks.is_empty();
                }
            }
        }
    }
}
