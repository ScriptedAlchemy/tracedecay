use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;

use super::super::ToolResult;
use super::super::binding::tool_dispatches_registered_project_reader;
use super::super::render;
use super::support;
use super::support::{project_registry_context, project_selector_present};

const WORKER_SETTLEMENT_GRACE: std::time::Duration = std::time::Duration::from_millis(25);

pub(in crate::mcp::tools) fn text_tool_result(text: &str) -> ToolResult {
    support::text_tool_result(text, Vec::new())
}

pub(in crate::mcp::tools) fn json_result(value: &Value) -> Result<ToolResult> {
    Ok(text_tool_result(&serde_json::to_string(value)?))
}

pub(super) fn boxed_send<'a, T, F>(
    future: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(future)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolDispatchStage {
    SchemaValidation,
    ProjectSelection,
    ApplicationRoute,
    Handler,
    Serialization,
}

impl McpToolDispatchStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema_validation",
            Self::ProjectSelection => "project_selection",
            Self::ApplicationRoute => "application_route",
            Self::Handler => "handler",
            Self::Serialization => "serialization",
        }
    }
}

/// One absolute deadline and cancellation signal for a complete MCP tool call.
///
/// Clones preserve the ingress deadline and deadline provenance; no stage can
/// restart the lifecycle budget or turn a fired deadline into cancellation.
#[derive(Clone)]
pub(crate) struct McpToolDispatchControl {
    tool_name: Arc<str>,
    budget: std::time::Duration,
    deadline: tracedecay_application::Deadline,
    deadline_at: tokio::time::Instant,
    cancellation: tracedecay_application::CancellationSignal,
    deadline_triggered: Arc<AtomicBool>,
}

impl McpToolDispatchControl {
    pub(crate) fn new(
        tool_name: impl Into<Arc<str>>,
        carried_deadline: Option<tracedecay_application::Deadline>,
        cancellation: tracedecay_application::CancellationSignal,
    ) -> Result<Self> {
        let tool_name = tool_name.into();
        let budget =
            super::dispatch_groups::tool_dispatch_budget(&tool_name, carried_deadline.as_ref())
                .ok_or_else(|| {
                    Self::deadline_error_for(
                        &tool_name,
                        std::time::Duration::ZERO,
                        McpToolDispatchStage::SchemaValidation,
                    )
                })?;
        let use_carried_deadline = carried_deadline.as_ref().is_some_and(|deadline| {
            crate::daemon_client::deadline_remaining(deadline).is_some_and(|remaining| {
                remaining <= super::dispatch_groups::tool_dispatch_ceiling(&tool_name)
            })
        });
        let now = tracedecay_application::clock::now_micros();
        let deadline = if use_carried_deadline {
            carried_deadline.ok_or_else(|| TraceDecayError::Config {
                message: "MCP dispatch deadline disappeared during admission".to_owned(),
            })?
        } else {
            let budget_micros =
                i64::try_from(budget.as_micros()).map_err(|_| TraceDecayError::Config {
                    message: "MCP dispatch deadline exceeds the domain clock".to_owned(),
                })?;
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                now.0.saturating_add(budget_micros),
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!("invalid MCP dispatch deadline: {error}"),
            })?
        };
        let deadline_at = tokio::time::Instant::now()
            .checked_add(budget)
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP dispatch deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        Ok(Self {
            tool_name,
            budget,
            deadline,
            deadline_at,
            cancellation,
            deadline_triggered: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn deadline(&self) -> tracedecay_application::Deadline {
        self.deadline.clone()
    }

    pub(crate) fn cancellation(&self) -> tracedecay_application::CancellationSignal {
        self.cancellation.clone()
    }

    pub(crate) fn cancel(&self, requested_at: tracedecay_domain::UtcMicros) -> bool {
        self.cancellation.cancel(requested_at)
    }

    pub(crate) fn check(&self, stage: McpToolDispatchStage) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(self.terminal_cancellation_error(stage));
        }
        if tokio::time::Instant::now() >= self.deadline_at {
            self.deadline_triggered.store(true, Ordering::Release);
            self.cancel(tracedecay_application::clock::now_micros());
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
        let cancellation = crate::daemon_client::wait_for_cancellation(self.cancellation.clone());
        tokio::pin!(deadline);
        tokio::pin!(cancellation);
        tokio::pin!(future);
        let terminal_error = tokio::select! {
            biased;
            () = &mut cancellation => self.terminal_cancellation_error(stage),
            () = &mut deadline => {
                self.deadline_triggered.store(true, Ordering::Release);
                self.cancel(tracedecay_application::clock::now_micros());
                self.deadline_error(stage)
            }
            result = &mut future => return result,
        };

        // Cancellation owns the admitted work until it settles. Dropping this
        // future here would detach any spawn_blocking child it is awaiting and
        // make the response's worker settlement unknowable. Handlers receive
        // the same cancellation signal and deadline, so cooperative work exits
        // promptly while non-interruptible blocking work remains joined.
        let _ = future.await;
        Err(terminal_error)
    }

    pub(crate) async fn run_value<T, F>(&self, stage: McpToolDispatchStage, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run(stage, async move { Ok(future.await) }).await
    }

    pub(crate) async fn run_retained<T, F>(
        &self,
        stage: McpToolDispatchStage,
        tasks: &crate::mcp::server::RetainedToolDispatchTasks,
        settlement: Arc<crate::mcp::server::DispatchExecutionSettlement>,
        future: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        self.check(stage)?;
        let mut result = tasks.spawn(settlement.observe(future))?;
        let deadline = tokio::time::sleep_until(self.deadline_at);
        let cancellation = crate::daemon_client::wait_for_cancellation(self.cancellation.clone());
        tokio::pin!(deadline);
        tokio::pin!(cancellation);
        let terminal_error = tokio::select! {
            biased;
            () = &mut cancellation => self.terminal_cancellation_error(stage),
            () = &mut deadline => {
                self.deadline_triggered.store(true, Ordering::Release);
                self.cancel(tracedecay_application::clock::now_micros());
                self.deadline_error(stage)
            }
            output = &mut result => return output.map_err(|error| TraceDecayError::Config {
                message: format!("retained MCP tool dispatch ended without a result: {error}"),
            })?,
        };

        // Cooperative handlers normally settle inside this grace and can say
        // "joined" in the terminal receipt. Non-cooperative work remains
        // owned by the server registry and reports "settling"; the response
        // deadline is never extended indefinitely by worker teardown.
        let _ = tokio::time::timeout(WORKER_SETTLEMENT_GRACE, &mut result).await;
        Err(terminal_error)
    }

    fn deadline_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        Self::deadline_error_for(&self.tool_name, self.budget, stage)
    }

    fn deadline_error_for(
        tool_name: &str,
        budget: std::time::Duration,
        stage: McpToolDispatchStage,
    ) -> TraceDecayError {
        TraceDecayError::project_route(
            "tool_dispatch_deadline_exceeded",
            true,
            format!(
                "tool '{tool_name}' exceeded its {}ms absolute deadline during {}",
                budget.as_millis(),
                stage.as_str(),
            ),
        )
    }

    fn cancellation_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        TraceDecayError::project_route(
            "tool_dispatch_cancelled",
            true,
            format!(
                "tool '{}' was cancelled during {}",
                self.tool_name,
                stage.as_str(),
            ),
        )
    }

    fn terminal_cancellation_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        if self.deadline_triggered.load(Ordering::Acquire) {
            self.deadline_error(stage)
        } else {
            self.cancellation_error(stage)
        }
    }
}

pub(crate) const INTERNAL_DAEMON_TOOL_NAMES: &[&str] = &[
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
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
    let context = boxed_send(project_registry_context(
        &args,
        &["project_path", "project_root"],
        global_db,
    ));
    let Some(context) = context.await.map_err(|error| {
        crate::mcp::project_route::ProjectRouteFailure::from_selection_error(&error).into_error()
    })?
    else {
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
    let graph = resolver(request.clone()).await?.ok_or_else(|| {
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
                }))?
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
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn deadline_remains_deadline_during_response_materialization() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancellation.deadline.fixture")
                .expect("cancellation");
        let control = McpToolDispatchControl::new("tracedecay_search", None, cancellation)
            .expect("dispatch control");
        let worker_cancellation = control.cancellation();
        let worker_started = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&worker_started);
        let settled = Arc::new(AtomicBool::new(false));
        let worker_settled = Arc::clone(&settled);
        let runner_control = control.clone();
        let runner = tokio::spawn(async move {
            runner_control
                .run(McpToolDispatchStage::Handler, async move {
                    started.notify_one();
                    crate::daemon_client::wait_for_cancellation(worker_cancellation).await;
                    worker_settled.store(true, Ordering::Release);
                    Ok(())
                })
                .await
        });
        worker_started.notified().await;
        tokio::time::advance(control.budget + std::time::Duration::from_millis(1)).await;
        let handler_error = runner
            .await
            .expect("dispatch runner joins")
            .expect_err("expired handler");
        assert_eq!(
            handler_error
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_deadline_exceeded")
        );
        assert!(
            settled.load(Ordering::Acquire),
            "deadline cancellation must join the owned worker"
        );

        let serialization_error = control
            .check(McpToolDispatchStage::Serialization)
            .expect_err("expired serialization");
        assert_eq!(
            serialization_error
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_deadline_exceeded"),
            "deadline-triggered cancellation must not be reclassified"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn non_cooperative_worker_returns_settling_receipt_state_and_remains_owned() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancellation.settling.fixture")
                .expect("cancellation");
        let control = McpToolDispatchControl::new("tracedecay_search", None, cancellation)
            .expect("dispatch control");
        let tasks = Arc::new(crate::mcp::server::RetainedToolDispatchTasks::new());
        let settlement = Arc::new(crate::mcp::server::DispatchExecutionSettlement::new());
        let worker_started = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::new(tokio::sync::Notify::new());
        let runner_control = control.clone();
        let runner_tasks = Arc::clone(&tasks);
        let runner_settlement = Arc::clone(&settlement);
        let started = Arc::clone(&worker_started);
        let release = Arc::clone(&worker_release);
        let runner = tokio::spawn(async move {
            runner_control
                .run_retained(
                    McpToolDispatchStage::Handler,
                    runner_tasks.as_ref(),
                    runner_settlement,
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(())
                    },
                )
                .await
        });

        worker_started.notified().await;
        tokio::time::advance(control.budget + std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(WORKER_SETTLEMENT_GRACE + std::time::Duration::from_millis(1)).await;
        let error = runner
            .await
            .expect("dispatch runner joins")
            .expect_err("non-cooperative handler exceeds deadline");
        assert_eq!(
            error.project_route_context().map(|context| context.0),
            Some("tool_dispatch_deadline_exceeded")
        );
        assert!(settlement.is_settling());

        worker_release.notify_one();
        tasks.shutdown().await;
        assert!(settlement.is_joined());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_non_cooperative_worker_is_reaped_by_shutdown_owner() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancellation.reap.fixture")
                .expect("cancellation");
        let control = McpToolDispatchControl::new("tracedecay_search", None, cancellation)
            .expect("dispatch control");
        let tasks = Arc::new(crate::mcp::server::RetainedToolDispatchTasks::new());
        let settlement = Arc::new(crate::mcp::server::DispatchExecutionSettlement::new());
        let worker_started = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::new(tokio::sync::Notify::new());
        let runner_control = control.clone();
        let runner_tasks = Arc::clone(&tasks);
        let runner_settlement = Arc::clone(&settlement);
        let started = Arc::clone(&worker_started);
        let release = Arc::clone(&worker_release);
        let runner = tokio::spawn(async move {
            runner_control
                .run_retained(
                    McpToolDispatchStage::Handler,
                    runner_tasks.as_ref(),
                    runner_settlement,
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(())
                    },
                )
                .await
        });

        worker_started.notified().await;
        control.cancel(tracedecay_application::clock::now_micros());
        tokio::task::yield_now().await;
        tokio::time::advance(WORKER_SETTLEMENT_GRACE + std::time::Duration::from_millis(1)).await;
        let error = runner
            .await
            .expect("dispatch runner joins")
            .expect_err("cancelled non-cooperative handler");
        assert_eq!(
            error.project_route_context().map(|context| context.0),
            Some("tool_dispatch_cancelled")
        );
        assert!(settlement.is_settling());

        let shutdown_tasks = Arc::clone(&tasks);
        let shutdown = tokio::spawn(async move {
            shutdown_tasks.shutdown().await;
        });
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must retain and join the unsettled handler"
        );
        worker_release.notify_one();
        shutdown.await.expect("shutdown owner joins");
        assert!(settlement.is_joined());
    }
}
