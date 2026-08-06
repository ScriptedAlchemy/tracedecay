use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::errors::{Result, TraceDecayError};

const SETTLEMENT_NOT_STARTED: u8 = 0;
const SETTLEMENT_SETTLING: u8 = 1;
const SETTLEMENT_JOINED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DispatchSettlement {
    NotStarted,
    Settling,
    Joined,
}

#[derive(Debug)]
pub(super) struct DispatchExecutionSettlement {
    state: AtomicU8,
}

impl DispatchExecutionSettlement {
    fn not_started() -> Self {
        Self {
            state: AtomicU8::new(SETTLEMENT_NOT_STARTED),
        }
    }

    fn mark_settling(&self) {
        self.state.store(SETTLEMENT_SETTLING, Ordering::Release);
    }

    fn mark_joined(&self) {
        self.state.store(SETTLEMENT_JOINED, Ordering::Release);
    }

    fn snapshot(&self) -> DispatchSettlement {
        match self.state.load(Ordering::Acquire) {
            SETTLEMENT_SETTLING => DispatchSettlement::Settling,
            SETTLEMENT_JOINED => DispatchSettlement::Joined,
            _ => DispatchSettlement::NotStarted,
        }
    }
}

#[derive(Debug)]
pub(super) struct DispatchFailure {
    error: TraceDecayError,
}

impl DispatchFailure {
    fn new(error: TraceDecayError) -> Self {
        Self { error }
    }

    pub(super) fn error(&self) -> &TraceDecayError {
        &self.error
    }

    #[cfg(test)]
    fn project_route_context(&self) -> Option<(&str, bool, &str)> {
        self.error.project_route_context()
    }
}

pub(super) struct RetainedDispatchOutcome<T> {
    pub(super) result: std::result::Result<T, DispatchFailure>,
    settlement: Arc<DispatchExecutionSettlement>,
}

impl<T> RetainedDispatchOutcome<T> {
    fn failed(error: TraceDecayError) -> Self {
        let settlement = Arc::new(DispatchExecutionSettlement::not_started());
        Self {
            result: Err(DispatchFailure::new(error)),
            settlement,
        }
    }

    pub(super) fn settlement(&self) -> DispatchSettlement {
        self.settlement.snapshot()
    }
}

fn dispatch_capacity_for_host() -> usize {
    let parallelism = std::thread::available_parallelism().map_or(4, usize::from);
    parallelism.saturating_mul(8).clamp(16, 256)
}

struct ActiveDispatch {
    cancellation: tracedecay_application::CancellationSignal,
    settlement: Arc<DispatchExecutionSettlement>,
}

struct RetainedDispatchState {
    tasks: tokio::task::JoinSet<Arc<DispatchExecutionSettlement>>,
    active: HashMap<tokio::task::Id, ActiveDispatch>,
}

/// Daemon-owned lifetime authority for admitted MCP handlers.
///
/// A caller may stop awaiting after pre-commit cancellation or deadline wins,
/// but the admitted task remains in this registry until it reaches a terminal
/// join. Shutdown closes admission, cancels every pre-commit signal, and joins
/// every retained task.
pub(super) struct RetainedDispatchRegistry {
    accepting: AtomicBool,
    capacity: usize,
    state: tokio::sync::Mutex<RetainedDispatchState>,
}

impl RetainedDispatchRegistry {
    pub(super) fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            capacity: dispatch_capacity_for_host(),
            state: tokio::sync::Mutex::new(RetainedDispatchState {
                tasks: tokio::task::JoinSet::new(),
                active: HashMap::new(),
            }),
        }
    }

    async fn spawn<T, F>(
        &self,
        cancellation: tracedecay_application::CancellationSignal,
        future: F,
    ) -> Result<(
        tokio::sync::oneshot::Receiver<Result<T>>,
        Arc<DispatchExecutionSettlement>,
    )>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let mut state = self.state.lock().await;
        Self::reap_finished(&mut state);
        if !self.accepting.load(Ordering::Acquire) {
            return Err(dispatch_shutdown_error());
        }
        if state.active.len() >= self.capacity {
            return Err(TraceDecayError::project_route(
                "tool_dispatch_saturated",
                true,
                "MCP retained dispatch capacity is exhausted",
            ));
        }

        let settlement = Arc::new(DispatchExecutionSettlement::not_started());
        let worker_settlement = Arc::clone(&settlement);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = state.tasks.spawn(async move {
            worker_settlement.mark_settling();
            let output = future.await;
            worker_settlement.mark_joined();
            let _ = sender.send(output);
            worker_settlement
        });
        state.active.insert(
            task.id(),
            ActiveDispatch {
                cancellation,
                settlement: Arc::clone(&settlement),
            },
        );
        Ok((receiver, settlement))
    }

    fn reap_finished(state: &mut RetainedDispatchState) {
        while let Some(joined) = state.tasks.try_join_next_with_id() {
            match joined {
                Ok((id, settlement)) => {
                    settlement.mark_joined();
                    state.active.remove(&id);
                }
                Err(error) => {
                    if let Some(active) = state.active.remove(&error.id()) {
                        active.settlement.mark_joined();
                    }
                    tracing::error!(error = %error, "retained MCP dispatch task failed");
                }
            }
        }
    }

    pub(super) async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        self.accepting.store(false, Ordering::Release);
        let requested_at = tracedecay_application::clock::now_micros();
        for active in state.active.values() {
            let _ = active.cancellation.cancel(requested_at);
        }
        while let Some(joined) = state.tasks.join_next_with_id().await {
            match joined {
                Ok((id, settlement)) => {
                    settlement.mark_joined();
                    state.active.remove(&id);
                }
                Err(error) => {
                    if let Some(active) = state.active.remove(&error.id()) {
                        active.settlement.mark_joined();
                    }
                    tracing::error!(error = %error, "retained MCP dispatch task failed");
                }
            }
        }
    }
}

pub(super) struct RetainedDispatchAuthority {
    cancellations: std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
    registry: RetainedDispatchRegistry,
    server: std::sync::Weak<super::McpServer>,
}

impl RetainedDispatchAuthority {
    pub(super) fn new(server: std::sync::Weak<super::McpServer>) -> Self {
        Self {
            cancellations: std::sync::Mutex::new(HashMap::new()),
            registry: RetainedDispatchRegistry::new(),
            server,
        }
    }

    pub(super) fn cancellations(
        &self,
    ) -> &std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>> {
        &self.cancellations
    }

    pub(super) fn registry(&self) -> &RetainedDispatchRegistry {
        &self.registry
    }

    pub(super) fn server(&self) -> std::sync::Weak<super::McpServer> {
        self.server.clone()
    }

    pub(super) async fn shutdown(&self) {
        self.registry.shutdown().await;
    }
}

pub(super) struct ApplicationCancellationRegistration<'a> {
    registry: &'a std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
    request_id: Option<String>,
}

impl<'a> ApplicationCancellationRegistration<'a> {
    pub(super) fn new(
        registry: &'a std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            registry,
            request_id,
        }
    }
}

impl Drop for ApplicationCancellationRegistration<'_> {
    fn drop(&mut self) {
        if let Some(request_id) = self.request_id.as_deref() {
            super::requests::recover_lock(self.registry).remove(request_id);
        }
    }
}

pub(super) struct PreparedDispatchControl<'a> {
    pub(super) request_id: Option<tracedecay_application::RequestId>,
    pub(super) control: DispatchControl,
    pub(super) _registration: ApplicationCancellationRegistration<'a>,
}

#[derive(Clone)]
pub(super) struct DispatchControl {
    tool_name: Arc<str>,
    deadline: tracedecay_application::Deadline,
    deadline_at: tokio::time::Instant,
    cancellation: tracedecay_application::CancellationSignal,
}

impl super::McpServer {
    pub(super) fn prepare_dispatch_control<'a>(
        &'a self,
        id: &serde_json::Value,
        tool_name: &str,
        memory_request_scope: &str,
        pre_cancelled: bool,
    ) -> Result<PreparedDispatchControl<'a>> {
        let request_id = super::application_surface_request_id(id, memory_request_scope)
            .and_then(|request_id| tracedecay_application::RequestId::new(request_id).ok());
        let cancellation_id = request_id.as_ref().map_or_else(
            || format!("cancellation.mcp.{tool_name}"),
            |request_id| format!("cancellation.{}", request_id.as_str()),
        );
        let cancellation = tracedecay_application::CancellationSignal::active(cancellation_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not create MCP dispatch cancellation: {error}"),
            })?;
        if pre_cancelled {
            let _ = cancellation.cancel(super::requests::mcp_now_micros());
        }
        let registered_request_id = super::requests::tool_supports_live_cancellation(tool_name)
            .then(|| {
                request_id
                    .as_ref()
                    .map(|request_id| request_id.as_str().to_owned())
            })
            .flatten();
        if let Some(request_id) = registered_request_id.as_ref() {
            super::requests::recover_lock(self.dispatch_authority.cancellations())
                .insert(request_id.clone(), cancellation.clone());
        }
        let registration = ApplicationCancellationRegistration::new(
            self.dispatch_authority.cancellations(),
            registered_request_id,
        );
        let application_surface =
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
        let source_edit = super::requests::is_source_edit_tool(tool_name);
        let controlled_read = super::requests::is_controlled_read_tool(tool_name);
        let carried_deadline = super::requests::dispatch_deadline_horizon_micros(
            application_surface.is_some() || source_edit,
            controlled_read || source_edit,
        )
        .and_then(|horizon| {
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                super::requests::mcp_now_micros().0.saturating_add(horizon),
            ))
            .ok()
        });
        let ceiling = crate::mcp::tools::handlers::tool_dispatch_ceiling(tool_name);
        let deadline = match carried_deadline {
            Some(deadline)
                if crate::daemon_client::deadline_remaining(&deadline)
                    .is_some_and(|remaining| remaining <= ceiling) =>
            {
                deadline
            }
            _ => {
                let micros =
                    i64::try_from(ceiling.as_micros()).map_err(|_| TraceDecayError::Config {
                        message: "MCP dispatch ceiling exceeds the domain clock".to_owned(),
                    })?;
                tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                    super::requests::mcp_now_micros().0.saturating_add(micros),
                ))
                .map_err(|error| TraceDecayError::Config {
                    message: format!("invalid MCP dispatch deadline: {error}"),
                })?
            }
        };
        let control = DispatchControl::new(tool_name, deadline, cancellation)?;
        Ok(PreparedDispatchControl {
            request_id,
            control,
            _registration: registration,
        })
    }
}

impl DispatchControl {
    pub(super) fn new(
        tool_name: impl Into<Arc<str>>,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationSignal,
    ) -> Result<Self> {
        let tool_name = tool_name.into();
        let remaining = crate::daemon_client::deadline_remaining(&deadline)
            .ok_or_else(|| dispatch_deadline_error(&tool_name, DispatchSettlement::NotStarted))?;
        let deadline_at = tokio::time::Instant::now()
            .checked_add(remaining)
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP dispatch deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        Ok(Self {
            tool_name,
            deadline,
            deadline_at,
            cancellation,
        })
    }

    pub(super) fn deadline(&self) -> tracedecay_application::Deadline {
        self.deadline.clone()
    }

    pub(super) fn cancellation(&self) -> tracedecay_application::CancellationSignal {
        self.cancellation.clone()
    }

    pub(super) async fn run_retained<T, F>(
        &self,
        registry: &RetainedDispatchRegistry,
        future: F,
    ) -> RetainedDispatchOutcome<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        if self.cancellation.is_cancelled() {
            return RetainedDispatchOutcome::failed(dispatch_cancelled_error(
                &self.tool_name,
                DispatchSettlement::NotStarted,
            ));
        }
        if tokio::time::Instant::now() >= self.deadline_at {
            let _ = self
                .cancellation
                .cancel(tracedecay_application::clock::now_micros());
            return RetainedDispatchOutcome::failed(dispatch_deadline_error(
                &self.tool_name,
                DispatchSettlement::NotStarted,
            ));
        }

        let (mut result, settlement) = match registry.spawn(self.cancellation.clone(), future).await
        {
            Ok(admitted) => admitted,
            Err(error) => return RetainedDispatchOutcome::failed(error),
        };
        let deadline = tokio::time::sleep_until(self.deadline_at);
        let cancellation = crate::daemon_client::wait_for_cancellation(self.cancellation.clone());
        tokio::pin!(deadline);
        tokio::pin!(cancellation);

        let outcome = tokio::select! {
            biased;
            () = &mut cancellation => {
                Err(DispatchFailure::new(dispatch_cancelled_error(
                    &self.tool_name,
                    settlement.snapshot(),
                )))
            }
            () = &mut deadline => {
                if self
                    .cancellation
                    .cancel(tracedecay_application::clock::now_micros())
                {
                    Err(DispatchFailure::new(dispatch_deadline_error(
                        &self.tool_name,
                        settlement.snapshot(),
                    )))
                } else {
                    receive_canonical_result(&mut result).await
                }
            }
            output = &mut result => {
                received_result(output)
            }
        };
        RetainedDispatchOutcome {
            result: outcome,
            settlement,
        }
    }
}

fn received_result<T>(
    output: std::result::Result<Result<T>, tokio::sync::oneshot::error::RecvError>,
) -> std::result::Result<T, DispatchFailure> {
    match output {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(DispatchFailure::new(error)),
        Err(error) => Err(DispatchFailure::new(TraceDecayError::Config {
            message: format!("retained MCP dispatch ended without a result: {error}"),
        })),
    }
}

async fn receive_canonical_result<T>(
    result: &mut tokio::sync::oneshot::Receiver<Result<T>>,
) -> std::result::Result<T, DispatchFailure> {
    received_result(result.await)
}

fn dispatch_cancelled_error(tool_name: &str, settlement: DispatchSettlement) -> TraceDecayError {
    TraceDecayError::project_route(
        "tool_dispatch_cancelled",
        true,
        format!(
            "tool '{tool_name}' was cancelled before commit; worker settlement is {settlement:?}"
        ),
    )
}

fn dispatch_deadline_error(tool_name: &str, settlement: DispatchSettlement) -> TraceDecayError {
    TraceDecayError::project_route(
        "tool_dispatch_deadline_exceeded",
        true,
        format!(
            "tool '{tool_name}' exceeded its absolute deadline before commit; worker settlement is {settlement:?}"
        ),
    )
}

fn dispatch_shutdown_error() -> TraceDecayError {
    TraceDecayError::project_route(
        "tool_dispatch_shutdown",
        true,
        "MCP server is shutting down and cannot admit another tool dispatch",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DispatchControl, DispatchSettlement, RetainedDispatchRegistry};

    fn deadline_after(duration: std::time::Duration) -> tracedecay_application::Deadline {
        let micros = i64::try_from(duration.as_micros()).expect("fixture duration");
        tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
            tracedecay_application::clock::now_micros()
                .0
                .saturating_add(micros),
        ))
        .expect("fixture deadline")
    }

    #[tokio::test]
    async fn cancellation_before_commit_returns_cancelled_while_the_worker_remains_owned() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.before-commit")
                .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_secs(60)),
            cancellation.clone(),
        )
        .expect("control");
        let worker_started = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&worker_started);
        let release = Arc::clone(&worker_release);
        let runner_registry = Arc::clone(&registry);
        let runner = tokio::spawn(async move {
            control
                .run_retained(&runner_registry, async move {
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, crate::errors::TraceDecayError>("not committed")
                })
                .await
        });

        worker_started.notified().await;
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let cancelled = runner.await.expect("dispatch runner");
        let settlement = Arc::clone(&cancelled.settlement);
        let failure = cancelled
            .result
            .expect_err("cancellation wins before commit");
        assert_eq!(
            failure.project_route_context().map(|context| context.0),
            Some("tool_dispatch_cancelled")
        );
        assert_eq!(settlement.snapshot(), DispatchSettlement::Settling);

        worker_release.notify_one();
        registry.shutdown().await;
        assert_eq!(settlement.snapshot(), DispatchSettlement::Joined);
    }

    #[tokio::test]
    async fn commit_claim_rejects_late_cancellation_and_returns_the_canonical_result() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.after-commit")
                .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_secs(60)),
            cancellation.clone(),
        )
        .expect("control");
        let commit_started = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&commit_started);
        let release = Arc::clone(&worker_release);
        let worker_cancellation = cancellation.clone();
        let runner_registry = Arc::clone(&registry);
        let runner = tokio::spawn(async move {
            control
                .run_retained(&runner_registry, async move {
                    assert!(worker_cancellation.try_begin_commit());
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, crate::errors::TraceDecayError>("committed")
                })
                .await
        });

        commit_started.notified().await;
        assert!(!cancellation.cancel(tracedecay_application::clock::now_micros()));
        assert!(
            !runner.is_finished(),
            "the committed worker owns canonical settlement"
        );
        worker_release.notify_one();
        let committed = runner.await.expect("dispatch runner");
        assert_eq!(
            committed.result.as_ref().expect("canonical result"),
            &"committed"
        );
        assert_eq!(committed.settlement(), DispatchSettlement::Joined);
        registry.shutdown().await;
    }
}
