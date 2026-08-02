use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;

use super::super::ToolResult;
use super::super::binding::tool_dispatches_registered_project_reader;
use super::super::execution::{McpToolExecutionAvailabilityV1, McpToolExecutionPolicyV1};
use super::super::render;
use super::support;
use super::support::{project_registry_context, project_selector_present};

pub(in crate::mcp::tools) fn text_tool_result(text: &str) -> ToolResult {
    support::text_tool_result(text, Vec::new())
}

pub(in crate::mcp::tools) fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&serde_json::to_string(value).unwrap_or_default())
}

pub(super) fn boxed_send<'a, T, F>(
    future: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(future)
}

/// The current lifecycle segment under the one absolute MCP dispatch deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolDispatchStage {
    SchemaValidation,
    ProjectSelection,
    WarmOpen,
    ApplicationRoute,
    Handler,
    Serialization,
}

/// What the lifecycle can truthfully say about request-owned worker cleanup at
/// the terminal response boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum McpToolWorkerSettlement {
    Joined = 0,
    Cancelled = 1,
    Indeterminate = 2,
    Leaked = 3,
}

impl McpToolWorkerSettlement {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Joined,
            1 => Self::Cancelled,
            2 => Self::Indeterminate,
            _ => Self::Leaked,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Cancelled => "cancelled",
            Self::Indeterminate => "indeterminate",
            Self::Leaked => "leaked",
        }
    }
}

impl McpToolDispatchStage {
    const fn as_u8(self) -> u8 {
        match self {
            Self::SchemaValidation => 0,
            Self::ProjectSelection => 1,
            Self::WarmOpen => 2,
            Self::ApplicationRoute => 3,
            Self::Handler => 4,
            Self::Serialization => 5,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema_validation",
            Self::ProjectSelection => "project_selection",
            Self::WarmOpen => "warm_open",
            Self::ApplicationRoute => "application_route",
            Self::Handler => "handler",
            Self::Serialization => "serialization",
        }
    }
}

/// Immutable request control created immediately after MCP tool-name lookup.
///
/// Every stage shares `deadline_at`; no nested layer gets to refresh a full
/// timeout. Cancellation is one live signal, so transport cancel, expiry, and
/// application invocation observe the same request state.
#[derive(Clone)]
pub(crate) struct McpToolDispatchControl {
    tool_name: Arc<str>,
    policy: McpToolExecutionPolicyV1,
    deadline: tracedecay_application::Deadline,
    deadline_at: tokio::time::Instant,
    cancellation: tracedecay_application::CancellationSignal,
    stage: Arc<AtomicU8>,
    owned_workers: Arc<AtomicUsize>,
    worker_settlement: Arc<AtomicU8>,
}

impl McpToolDispatchControl {
    pub(crate) fn new(
        tool_name: impl Into<Arc<str>>,
        policy: McpToolExecutionPolicyV1,
        cancellation: tracedecay_application::CancellationSignal,
    ) -> Result<Self> {
        let tool_name = tool_name.into();
        if let McpToolExecutionAvailabilityV1::Unavailable { reason_code } = policy.availability() {
            return Err(TraceDecayError::mcp_tool_dispatch(
                "tool_unavailable",
                McpToolDispatchStage::SchemaValidation.as_str(),
                true,
                format!("tool '{tool_name}' is unavailable: {reason_code}"),
            ));
        }
        let horizon = std::time::Duration::from_millis(policy.deadline_millis());
        let now = tracedecay_application::clock::now_micros();
        let horizon_micros = i64::try_from(horizon.as_micros()).unwrap_or(i64::MAX);
        let deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
            now.0.saturating_add(horizon_micros),
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid MCP dispatch deadline: {error}"),
        })?;
        let deadline_at = tokio::time::Instant::now()
            .checked_add(horizon)
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP dispatch deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        Ok(Self {
            tool_name,
            policy,
            deadline,
            deadline_at,
            cancellation,
            stage: Arc::new(AtomicU8::new(
                McpToolDispatchStage::SchemaValidation.as_u8(),
            )),
            owned_workers: Arc::new(AtomicUsize::new(0)),
            worker_settlement: Arc::new(AtomicU8::new(McpToolWorkerSettlement::Joined as u8)),
        })
    }

    pub(crate) fn deadline(&self) -> tracedecay_application::Deadline {
        self.deadline.clone()
    }

    pub(crate) fn cancellation(&self) -> tracedecay_application::CancellationSignal {
        self.cancellation.clone()
    }

    pub(crate) fn policy(&self) -> &McpToolExecutionPolicyV1 {
        &self.policy
    }

    pub(crate) fn cancel(&self, requested_at: tracedecay_domain::UtcMicros) -> bool {
        self.cancellation.cancel(requested_at)
    }

    /// The terminal worker settlement state for the execution receipt.
    pub(crate) fn worker_settlement(&self) -> McpToolWorkerSettlement {
        if self.owned_workers.load(Ordering::Acquire) != 0 {
            return McpToolWorkerSettlement::Leaked;
        }
        McpToolWorkerSettlement::from_u8(self.worker_settlement.load(Ordering::Acquire))
    }

    pub(crate) fn check(&self, stage: McpToolDispatchStage) -> Result<()> {
        self.stage.store(stage.as_u8(), Ordering::Release);
        if self.cancellation.is_cancelled() {
            self.worker_settlement
                .store(McpToolWorkerSettlement::Cancelled as u8, Ordering::Release);
            return Err(self.cancelled_error(stage));
        }
        if tokio::time::Instant::now() >= self.deadline_at {
            self.cancel(tracedecay_application::clock::now_micros());
            self.worker_settlement
                .store(McpToolWorkerSettlement::Cancelled as u8, Ordering::Release);
            return Err(self.deadline_error(stage));
        }
        Ok(())
    }

    pub(crate) async fn run<T, F>(&self, stage: McpToolDispatchStage, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.check(stage)?;
        let deadline = tokio::time::sleep_until(self.deadline_at);
        tokio::pin!(deadline);
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => result,
            () = &mut deadline => {
                self.cancel(tracedecay_application::clock::now_micros());
                self.worker_settlement
                    .store(McpToolWorkerSettlement::Indeterminate as u8, Ordering::Release);
                Err(self.deadline_error(stage))
            }
            () = self.cancellation.cancelled() => {
                self.worker_settlement
                    .store(McpToolWorkerSettlement::Indeterminate as u8, Ordering::Release);
                Err(self.cancelled_error(stage))
            },
        }
    }

    /// Run a lifecycle operation whose successful value is not itself a
    /// TraceDecay result (for example a completed JSON-RPC response).
    pub(crate) async fn run_value<T, F>(&self, stage: McpToolDispatchStage, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.check(stage)?;
        let deadline = tokio::time::sleep_until(self.deadline_at);
        tokio::pin!(deadline);
        tokio::pin!(future);
        tokio::select! {
            value = &mut future => Ok(value),
            () = &mut deadline => {
                self.cancel(tracedecay_application::clock::now_micros());
                self.worker_settlement
                    .store(McpToolWorkerSettlement::Indeterminate as u8, Ordering::Release);
                Err(self.deadline_error(stage))
            }
            () = self.cancellation.cancelled() => {
                self.worker_settlement
                    .store(McpToolWorkerSettlement::Indeterminate as u8, Ordering::Release);
                Err(self.cancelled_error(stage))
            },
        }
    }

    /// Wait for an owned worker under the same absolute deadline. A terminal
    /// control event aborts *and joins* it before returning, so a cancelled
    /// request cannot retain detached work behind the caller's response.
    pub(crate) async fn run_owned<T>(
        &self,
        stage: McpToolDispatchStage,
        mut worker: tokio::task::JoinHandle<Result<T>>,
    ) -> Result<T> {
        self.owned_workers.fetch_add(1, Ordering::AcqRel);
        let result = match self.check(stage) {
            Err(error) => {
                worker.abort();
                let _ = worker.await;
                Err(error)
            }
            Ok(()) => {
                let deadline = tokio::time::sleep_until(self.deadline_at);
                tokio::pin!(deadline);
                tokio::select! {
                    result = &mut worker => match result {
                        Ok(result) => result,
                        Err(error) => Err(TraceDecayError::mcp_tool_dispatch(
                            "tool_dispatch_worker_failed",
                            stage.as_str(),
                            true,
                            format!("tool '{}' worker did not complete: {error}", self.tool_name),
                        )),
                    },
                    () = &mut deadline => {
                        self.cancel(tracedecay_application::clock::now_micros());
                        worker.abort();
                        let _ = worker.await;
                        Err(self.deadline_error(stage))
                    }
                    () = self.cancellation.cancelled() => {
                        worker.abort();
                        let _ = worker.await;
                        Err(self.cancelled_error(stage))
                    }
                }
            }
        };
        self.owned_workers.fetch_sub(1, Ordering::AcqRel);
        self.worker_settlement
            .store(McpToolWorkerSettlement::Joined as u8, Ordering::Release);
        result
    }

    fn deadline_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_deadline_exceeded",
            stage.as_str(),
            true,
            format!(
                "tool '{}' exceeded its {}ms absolute dispatch deadline",
                self.tool_name,
                self.policy.deadline_millis(),
            ),
        )
    }

    fn cancelled_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_cancelled",
            stage.as_str(),
            true,
            format!(
                "tool '{}' was cancelled during {}",
                self.tool_name,
                stage.as_str()
            ),
        )
    }
}

pub(super) const INTERNAL_DAEMON_TOOL_NAMES: &[&str] = &[
    "tracedecay_admin_branch_add",
    "tracedecay_admin_cli",
    "tracedecay_admin_project",
    "tracedecay_admin_sync",
    "tracedecay_hook_runtime",
];

pub(super) fn rejected_tool_project_selector_present(tool_name: &str, args: &Value) -> bool {
    let top_level_path_keys = if tool_name.starts_with("tracedecay_lcm_") {
        &["project_path"][..]
    } else {
        &["project_path", "project_root"][..]
    };
    project_selector_present(args, top_level_path_keys)
}

pub(crate) async fn selected_registered_project_reader(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    dispatch_control: Option<&McpToolDispatchControl>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
    let context = boxed_send(project_registry_context(
        &args,
        &["project_path", "project_root"],
        global_db,
    ));
    let selection = async {
        context.await.map_err(|error| {
            crate::mcp::project_route::ProjectRouteFailure::from_selection_error(&error)
                .into_error()
        })
    };
    let context = match dispatch_control {
        Some(control) => {
            control
                .run(McpToolDispatchStage::ProjectSelection, selection)
                .await
        }
        None => selection.await,
    };
    let Some(context) = context? else {
        return Ok(None);
    };

    let Some(resolver) = resolver else {
        return Err(TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "registered project graph resolver is unavailable",
        ));
    };
    let requested_path = args
        .get("project_selector")
        .and_then(Value::as_object)
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| args.get("project_path"))
        .or_else(|| args.get("project_root"))
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(|path| {
            crate::worktree::git_worktree_root(path).or_else(|| path.canonicalize().ok())
        })
        .unwrap_or_else(|| Path::new(&context.project.canonical_root).to_path_buf());
    let request = crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
        context.clone(),
        requested_path.clone(),
    );
    let graph = match dispatch_control {
        Some(control) => {
            control
                .run(McpToolDispatchStage::WarmOpen, resolver(request.clone()))
                .await
        }
        None => resolver(request.clone()).await,
    }?
    .ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            format!(
                "registered project '{}' is not mounted for workspace {}",
                context.project.project_id,
                requested_path.display()
            ),
        )
    })?;
    let scope = crate::mcp::scope::resolve_query_scope(&context, &requested_path)
        .map_err(|error| error.into_route_failure().into_error())?;
    Ok(Some(crate::mcp::project_route::ResolvedProjectRoute {
        graph,
        owner: context,
        requested_root: requested_path,
        requested_git_common_dir: request.requested_git_common_dir,
        requested_branch: request.requested_branch,
        scope,
    }))
}

pub(super) fn handle_retrieve(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "missing required parameter: handle (copy the exact `handle` value from a truncated MCP response envelope)"
                        .to_string(),
            })?;
    let payload = match retrieve_response_handle(cg.project_root(), handle, current_timestamp())? {
        ResponseHandleLookup::Found(record) => {
            // Retrieval never truncates: the stored content is by definition
            // larger than the response cap, so neither output path may route
            // through the truncating envelope again. Markdown (default)
            // returns the stored text verbatim under a small header; JSON
            // serializes the payload directly.
            let text = if render::wants_json(args) {
                serde_json::to_string(&json!({
                    "handle": record.handle,
                    "expired": false,
                    "original_chars": record.original_chars(),
                    "created_at": record.created_at,
                    "expires_at": record.expires_at,
                    "content": record.content,
                }))
                .unwrap_or_default()
            } else {
                format!(
                    "## Retrieved Response\n**handle:** `{}` ({} chars, expires at {})\n\n{}",
                    record.handle,
                    record.original_chars(),
                    record.expires_at,
                    record.content,
                )
            };
            return Ok(text_tool_result(&text));
        }
        ResponseHandleLookup::Missing => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_not_found",
            "message": "Response handle was not found in this project's local cache.",
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
        }),
        ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        } => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_expired",
            "message": format!(
                "Response handle expired at {expires_at} and was removed from this project's local cache."
            ),
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
            "created_at": created_at,
            "expires_at": expires_at,
        }),
    };
    Ok(support::tool_json(Some(cg.project_root()), args, &payload))
}

#[cfg(test)]
mod dispatch_control_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tracedecay_application::CancellationSignal;
    use tracedecay_domain::UtcMicros;

    use super::{McpToolDispatchControl, McpToolDispatchStage};
    use crate::mcp::tools::execution::McpToolExecutionPolicyV1;

    #[tokio::test]
    async fn one_absolute_deadline_covers_the_entire_current_stage() {
        let cancellation = CancellationSignal::active("cancel.dispatch.deadline").unwrap();
        let control = McpToolDispatchControl::new(
            "tracedecay_deadline_fixture",
            McpToolExecutionPolicyV1::interactive_read(20),
            cancellation,
        )
        .unwrap();

        let error = control
            .run(McpToolDispatchStage::ProjectSelection, async {
                std::future::pending::<crate::errors::Result<()>>().await
            })
            .await
            .unwrap_err();
        let (reason_code, stage, retryable, _) = error
            .mcp_tool_dispatch_context()
            .expect("deadline must retain structured MCP dispatch context");
        assert_eq!(reason_code, "tool_dispatch_deadline_exceeded");
        assert_eq!(stage, "project_selection");
        assert!(retryable);
    }

    #[tokio::test]
    async fn serialization_value_is_covered_by_the_same_deadline() {
        let cancellation = CancellationSignal::active("cancel.dispatch.serialization").unwrap();
        let control = McpToolDispatchControl::new(
            "tracedecay_serialization_fixture",
            McpToolExecutionPolicyV1::interactive_read(20),
            cancellation,
        )
        .unwrap();

        let error = control
            .run_value(McpToolDispatchStage::Serialization, async {
                std::future::pending::<()>().await
            })
            .await
            .unwrap_err();
        let (reason_code, stage, retryable, _) = error
            .mcp_tool_dispatch_context()
            .expect("serialization deadline must retain structured MCP dispatch context");
        assert_eq!(reason_code, "tool_dispatch_deadline_exceeded");
        assert_eq!(stage, "serialization");
        assert!(retryable);
    }

    #[tokio::test]
    async fn cancellation_aborts_and_joins_an_owned_worker() {
        let cancellation = CancellationSignal::active("cancel.dispatch.worker").unwrap();
        let control = McpToolDispatchControl::new(
            "tracedecay_worker_fixture",
            McpToolExecutionPolicyV1::interactive_read(1_000),
            cancellation,
        )
        .unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_started = Arc::clone(&started);
        let worker_stopped = Arc::clone(&stopped);
        let worker = tokio::spawn(async move {
            struct Stop(Arc<AtomicBool>);
            impl Drop for Stop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _stop = Stop(worker_stopped);
            worker_started.notify_one();
            std::future::pending::<()>().await;
            Ok::<(), crate::errors::TraceDecayError>(())
        });
        started.notified().await;

        let cancellation_control = control.clone();
        let dispatch = tokio::spawn(async move {
            cancellation_control
                .run_owned(McpToolDispatchStage::Handler, worker)
                .await
        });
        tokio::task::yield_now().await;
        control.cancel(UtcMicros(73));

        let error = tokio::time::timeout(Duration::from_millis(100), dispatch)
            .await
            .expect("dispatch cancellation must settle")
            .expect("dispatch task must not panic")
            .unwrap_err();
        let (reason_code, stage, retryable, _) = error
            .mcp_tool_dispatch_context()
            .expect("cancellation must retain structured MCP dispatch context");
        assert_eq!(reason_code, "tool_dispatch_cancelled");
        assert_eq!(stage, "handler");
        assert!(retryable);
        assert!(
            stopped.load(Ordering::Acquire),
            "owned worker must be aborted and joined before dispatch returns"
        );
    }
}
